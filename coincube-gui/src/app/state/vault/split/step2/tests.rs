//! Split step-2 panel layer (#568 B3b-2b-1) tests: the journal-lock ordering
//! against a real Split journal and its real lock, the restart decision
//! (reconcile only once a step-2 submission is recorded), the user copy and
//! the live replay label, and the D1 dormancy of this module.
//!
//! The journal is the production one, written through
//! `claim_workflow::Controller`'s Split API in a private temporary directory.
//! Step 1 and step 2 are real core constructions of a P2PKH foreign wallet
//! (its scriptSigs change every txid), signed by rust-bitcoin's reference
//! PSBT signer. Observations are synthetic bundles.
use super::*;
use crate::{
    app::state::vault::claim::ForkWindow,
    services::{
        claim_coordinator::Error as CoordinatorError,
        claim_observation::{CollectedAssessment, FailureKind, ObservationBundle},
        claim_workflow::Phase,
        split_evidence::SplitEvidenceSource,
    },
};
use coincube_core::{
    chain::ChainId,
    claim::{
        Assessment, BitcoinObservation, BlockRef, DeploymentObservation, DeploymentState,
        ForkObservation, ForkTransactionPresence, Policy, PreflightTips, TransactionLocation,
    },
    foreign_split::{
        create_split_step1, create_split_step2, finalize_split_step1, finalize_split_step2,
        SplitBranch, SplitInputs, SplitSource, SplitStep2, SplitStep2Inputs,
    },
    miniscript::{
        bitcoin::{
            absolute::LockTime,
            bip32::{DerivationPath, Xpriv, Xpub},
            hashes::Hash,
            secp256k1::Secp256k1,
            transaction, Amount, BlockHash, Network, OutPoint, ScriptBuf, Transaction, TxIn, TxOut,
        },
        Descriptor,
    },
};
use std::{
    path::Path,
    str::FromStr,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

const FORK: u64 = 900;
const TIP: u32 = 960;
const TARGET: &str = "btcb2-target-cube";
static DIRS: AtomicUsize = AtomicUsize::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "split-panel-step2-{}-{}",
            std::process::id(),
            DIRS.fetch_add(1, Ordering::Relaxed)
        ));
        let directory = root.join("journal");
        claim_workflow::prepare_directory(&directory).unwrap();
        Self(directory)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        if let Some(root) = self.0.parent() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn context() -> Context {
    Context {
        generation: 7,
        account: "synthetic-account".into(),
        provider: "synthetic-provider".into(),
    }
}
fn policy() -> Policy {
    Policy {
        max_observation_age_seconds: 60,
        expiry_margin_seconds: 600,
    }
}
fn hash(n: u8) -> BlockHash {
    BlockHash::from_byte_array([n; 32])
}

struct Wallet {
    source: SplitSource,
    signer: Xpriv,
}
fn wallet() -> Wallet {
    let secp = Secp256k1::new();
    let signer = Xpriv::new_master(Network::Bitcoin, &[4; 32]).unwrap();
    let path = "m/44'/0'/0'";
    let child = signer
        .derive_priv(&secp, &DerivationPath::from_str(path).unwrap())
        .unwrap();
    let key = format!(
        "[{}/{}]{}",
        signer.fingerprint(&secp),
        path.trim_start_matches("m/"),
        Xpub::from_priv(&secp, &child)
    );
    let branch = |b: u32| Descriptor::from_str(&format!("pkh({key}/{b}/*)")).unwrap();
    Wallet {
        source: SplitSource::new(branch(0), Some(branch(1))).unwrap(),
        signer,
    }
}
fn coin(source: &SplitSource, branch: SplitBranch, index: u32, sats: u64) -> SplitCoin {
    let descriptor = match branch {
        SplitBranch::External => source.external(),
        SplitBranch::Internal => source.internal().unwrap(),
    };
    let previous = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([index as u8 + 1; 32]), 0),
            ..TxIn::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(sats),
            script_pubkey: descriptor
                .at_derivation_index(index)
                .unwrap()
                .script_pubkey(),
        }],
    };
    let block = BlockRef {
        height: FORK - 10,
        hash: hash(0x33),
    };
    SplitCoin {
        outpoint: OutPoint::new(previous.compute_txid(), 0),
        branch,
        index,
        previous,
        bitcoin_block: Some(block),
        btcb2_block: Some(block),
    }
}
fn coins(wallet: &Wallet) -> Vec<SplitCoin> {
    vec![
        coin(&wallet.source, SplitBranch::External, 0, 150_000),
        coin(&wallet.source, SplitBranch::Internal, 1, 70_000),
    ]
}
fn step1(wallet: &Wallet) -> SplitStep1 {
    create_split_step1(
        &SplitInputs {
            chain: ChainId::Bitcoin,
            source: &wallet.source,
            coins: &coins(wallet),
            fork_height: FORK,
            destination: 5,
        },
        2,
        LockTime::from_height(TIP).unwrap(),
        TIP,
        hash(7),
    )
    .unwrap()
}
fn sign1(step1: &SplitStep1, wallet: &Wallet) -> VerifiedSplitStep1 {
    let secp = Secp256k1::new();
    let mut psbt = step1.psbt().clone();
    psbt.sign(&wallet.signer, &secp).unwrap();
    finalize_split_step1(step1, &psbt, &secp).unwrap()
}
/// A P2WSH target script standing in for the Vault's receive address.
fn target_script() -> ScriptBuf {
    ScriptBuf::new_p2wsh(&coincube_core::miniscript::bitcoin::WScriptHash::from_byte_array([9; 32]))
}
fn step2(wallet: &Wallet, step1: &SplitStep1) -> SplitStep2 {
    create_split_step2(
        &SplitStep2Inputs {
            chain: ChainId::BitcoinBlake2b,
            source: &wallet.source,
            coins: &coins(wallet),
            fork_height: FORK,
            claimed: &step1.claimed_prevouts(),
            target: &target_script(),
        },
        2,
        LockTime::from_height(100).unwrap(),
        100,
    )
    .unwrap()
}

/// A fresh bundle: step 1 (`txid`) six deep on Bitcoin, absent on BTCB2.
fn six_deep(txid: Txid) -> CollectedAssessment {
    let block = BlockRef {
        height: 100,
        hash: hash(4),
    };
    let tip = BlockRef {
        height: 105,
        hash: hash(1),
    };
    let fork = BlockRef {
        height: 100,
        hash: hash(2),
    };
    CollectedAssessment {
        generation: 7,
        assessment: Assessment::Unknown,
        observations: ObservationBundle {
            bitcoin_transaction: TransactionObservation::Confirmed { txid, block },
            bitcoin: BitcoinObservation {
                chain: ChainId::Bitcoin,
                tip,
                location: TransactionLocation::Confirmed {
                    txid,
                    block,
                    best_chain_hash_at_height: block.hash,
                },
                observed_at: 10_000,
            },
            fork: ForkObservation {
                chain: ChainId::BitcoinBlake2b,
                step1_txid: txid,
                step1_presence: ForkTransactionPresence::NotObserved,
                tip: fork,
                median_time_past: 8_000,
                observed_at: 10_000,
            },
            deployment: DeploymentObservation {
                chain: ChainId::BitcoinBlake2b,
                tip: fork,
                state: DeploymentState::Flagday {
                    height: 90,
                    expiry_time: 20_000,
                    active: true,
                },
                observed_at: 10_000,
            },
            preflight: PreflightTips { bitcoin: tip, fork },
        },
    }
}
fn observe_under(controller: &mut Controller, context: &Context, txid: Txid, absent: bool) {
    let mut bundle = six_deep(txid);
    if absent {
        bundle.observations.bitcoin_transaction = TransactionObservation::Absent;
        bundle.observations.bitcoin.location = TransactionLocation::Unconfirmed;
    }
    let ticket = controller.begin_check(context).unwrap();
    controller
        .apply_observation(ticket, context, Ok(bundle), policy(), 10_000)
        .unwrap();
}

/// A Split journal whose step 1 is recorded as submitted and seen six deep,
/// and, with `step2`, a recorded step-2 submission of signed bytes.
struct Journal {
    temp: Temp,
    wallet: Wallet,
    step1: SplitStep1,
}
impl Journal {
    fn new(with_step2: bool) -> Self {
        Self::under(with_step2, &context())
    }
    /// [`Self::new`] journaled under `context` (a production session's).
    fn under(with_step2: bool, context: &Context) -> Self {
        let temp = Temp::new();
        let wallet = wallet();
        let step1 = step1(&wallet);
        let signed = sign1(&step1, &wallet);
        let tracked = signed.transaction().compute_txid();
        let mut c = Controller::create_split(
            &temp.0,
            TARGET.into(),
            &step1,
            &signed,
            FORK,
            context.clone(),
        )
        .unwrap();
        observe_under(&mut c, context, tracked, true);
        c.record_split_broadcast_intent(context, &signed, policy(), 10_000)
            .unwrap();
        observe_under(&mut c, context, tracked, false);
        assert_eq!(c.phase(), Phase::Tracking);
        if with_step2 {
            c.record_split_target(context, 3, target_script()).unwrap();
            let construction = step2(&wallet, &step1);
            observe_under(&mut c, context, tracked, false);
            c.prepare_split_step2(context, &construction, policy(), 10_000)
                .unwrap();
            let secp = Secp256k1::new();
            let mut psbt = construction.psbt().clone();
            psbt.sign(&wallet.signer, &secp).unwrap();
            let verified =
                finalize_split_step2(&construction, &coins(&wallet), &wallet.source, &psbt, &secp)
                    .unwrap();
            observe_under(&mut c, context, tracked, false);
            c.record_split_step2_broadcast_intent(context, &verified, policy(), 10_000)
                .unwrap();
        }
        Self {
            temp,
            wallet,
            step1,
        }
    }
    /// [`Self::new`] with a recorded step 2 whose latest attempt is recorded
    /// as having come back unaccepted (P3-3), and, with `observed`, a step 2
    /// since seen on BTCB2.
    fn returned(observed: bool) -> Self {
        let journal = Self::new(true);
        let mut c = journal.lock();
        c.record_split_step2_returned(&context()).unwrap();
        if observed {
            let txid = c.recorded_split_step2().unwrap().compute_txid();
            c.record_split_step2_observed(&context(), TransactionObservation::Unconfirmed { txid })
                .unwrap();
        }
        drop(c);
        journal
    }
    /// [`Self::returned`] with its resends already at the limit (P3-3): a
    /// journal only a long run of resends could produce, written directly.
    fn at_resend_limit() -> Self {
        let journal = Self::returned(false);
        let wtxid = journal
            .lock()
            .recorded_split_step2()
            .unwrap()
            .compute_wtxid()
            .to_string();
        let path = journal.temp.0.join("intent.json");
        let mut intent: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        intent["split"]["step2_resubmissions"] = serde_json::Value::Array(vec![
            serde_json::json!({ "wtxid": wtxid });
            claim_workflow::MAX_SPLIT_STEP2_RESUBMISSIONS
        ]);
        std::fs::write(&path, serde_json::to_vec(&intent).unwrap()).unwrap();
        let c = journal.lock();
        assert_eq!(
            c.split_step2_resubmissions(),
            claim_workflow::MAX_SPLIT_STEP2_RESUBMISSIONS
        );
        assert!(c.split_step2_returned() && !c.split_step2_observed());
        drop(c);
        journal
    }
    fn digest(&self) -> sha256::Hash {
        self.step1.source().digest()
    }
    fn lock(&self) -> Controller {
        Controller::reopen_settling_blocking(
            &self.temp.0,
            &claim_workflow::split_identity(TARGET.into(), self.digest()),
            context(),
        )
        .unwrap()
    }
    fn open(&self) -> Step2Open {
        Step2Open {
            directory: self.temp.0.clone(),
            target_cube: TARGET.into(),
            construction: self.step1.clone(),
            verified: sign1(&self.step1, &self.wallet),
            fork_height: FORK,
        }
    }
}

/// A step-1 driver holding the real journal lock (a reopened controller).
struct HeldStep1 {
    _controller: Controller,
    dropped: Arc<AtomicUsize>,
}
impl Drop for HeldStep1 {
    fn drop(&mut self) {
        self.dropped.fetch_add(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl Step1Driver for HeldStep1 {
    fn phase(&self) -> Phase {
        Phase::Tracking
    }
    fn revoke_handle(&self) -> RevokeHandle {
        Arc::new(|| {})
    }
    async fn review(&mut self, _: &Context) -> Result<step1::ReviewView, CoordinatorError> {
        unreachable!()
    }
    async fn submit(&mut self, _: &Context) -> Result<Outcome, CoordinatorError> {
        unreachable!()
    }
    async fn reconcile(&mut self, _: &Context) -> Result<Status, CoordinatorError> {
        unreachable!()
    }
    async fn recover(&mut self, _: &Context) -> Result<step1::Recovery, CoordinatorError> {
        unreachable!()
    }
    async fn acknowledge(&mut self, _: &Context) -> Result<(), CoordinatorError> {
        unreachable!()
    }
    async fn resend(&mut self, _: &Context) -> Result<Outcome, CoordinatorError> {
        unreachable!()
    }
}

/// A step-2 preparation holding the real journal lock.
struct HeldPrep {
    _controller: Controller,
}
#[async_trait]
impl Step2Prep for HeldPrep {
    fn revoke_handle(&self) -> RevokeHandle {
        Arc::new(|| {})
    }
    fn needs_reservation(&self) -> bool {
        true
    }
    async fn check(&mut self, _: &Context) -> Result<CannotReplay, Step2Refusal> {
        unreachable!()
    }
    async fn ensure_target(&mut self, _: &Context) -> Result<u32, Step2Refusal> {
        unreachable!()
    }
    async fn build(&mut self, _: &Context, _: Vec<SplitCoin>) -> Result<Psbt, Step2Refusal> {
        unreachable!()
    }
    fn verify_signed(&self, _: &Psbt, _: &[SplitCoin]) -> Result<bool, Step2Refusal> {
        unreachable!()
    }
    fn finish(
        self: Box<Self>,
        _: &Context,
        _: &Psbt,
        _: &[SplitCoin],
    ) -> Result<Box<dyn Step2Coord>, FinishRefusal> {
        unreachable!()
    }
}
struct HeldRecon {
    _controller: Controller,
}
#[async_trait]
impl Step2Recon for HeldRecon {
    fn revoke_handle(&self) -> RevokeHandle {
        Arc::new(|| {})
    }
    fn recorded_outcome(&self) -> Option<Outcome> {
        None
    }
    async fn reconcile(
        &mut self,
        _: &Context,
    ) -> Result<(Status, TransactionObservation, Step1AfterStep2), Step2Refusal> {
        unreachable!()
    }
    async fn complete(&mut self, _: &Context) -> Result<SplitCompletion, Step2Refusal> {
        unreachable!()
    }
    async fn completion_stands(&mut self, _: &Context) -> Result<CompletionStanding, Step2Refusal> {
        unreachable!()
    }
    async fn review_reconfirmation(
        &mut self,
        _: &Context,
    ) -> Result<ReconfirmationView, Step2Refusal> {
        unreachable!()
    }
    async fn confirm_reconfirmation(&mut self, _: &Context) -> Result<(), Step2Refusal> {
        unreachable!()
    }
}

/// A reopened coordinator holding the real journal lock (P3-3).
struct HeldCoord {
    _controller: Controller,
}
#[async_trait]
impl Step2Coord for HeldCoord {
    fn revoke_handle(&self) -> RevokeHandle {
        Arc::new(|| {})
    }
    fn recorded_outcome(&self) -> Option<Outcome> {
        None
    }
    async fn review(&mut self, _: &Context) -> Result<Step2ReviewView, Step2Refusal> {
        unreachable!()
    }
    async fn submit(&mut self, _: &Context) -> Result<Outcome, Step2Refusal> {
        unreachable!()
    }
    async fn reconcile(
        &mut self,
        _: &Context,
    ) -> Result<(Status, TransactionObservation, Step1AfterStep2), Step2Refusal> {
        unreachable!()
    }
    async fn review_resend(&mut self, _: &Context) -> Result<Step2ResendView, Step2Refusal> {
        unreachable!()
    }
    async fn confirm_resend(&mut self, _: &Context) -> Result<Outcome, Step2Refusal> {
        unreachable!()
    }
    async fn review_reconfirmation(
        &mut self,
        _: &Context,
    ) -> Result<ReconfirmationView, Step2Refusal> {
        unreachable!()
    }
    async fn confirm_reconfirmation(&mut self, _: &Context) -> Result<(), Step2Refusal> {
        unreachable!()
    }
}

/// A port that opens the real journal (and so needs its lock) and counts
/// what it was asked to open.
struct Port {
    directory: PathBuf,
    digest: sha256::Hash,
    preparations: AtomicUsize,
    reconcilers: AtomicUsize,
    /// P3-3: coordinators reopened for a resend.
    uncertain: AtomicUsize,
    /// Reopening for a resend refuses with this.
    refuse_uncertain: std::sync::Mutex<Option<Step2Refusal>>,
    /// The session account the port was built for.
    account: &'static str,
}
impl Port {
    fn lock(&self) -> Result<Controller, Step2Refusal> {
        Controller::reopen_settling_blocking(
            &self.directory,
            &claim_workflow::split_identity(TARGET.into(), self.digest),
            context(),
        )
        .map_err(|error| Step2Refusal::retry(format!("{error:?}")))
    }
}
#[async_trait]
impl Step2Port for Port {
    fn context(&self) -> Context {
        let mut context = context();
        context.account = self.account.into();
        context
    }
    fn identity(&self) -> PortIdentity {
        PortIdentity {
            context: Step2Port::context(self),
            daemon: 0,
        }
    }
    fn open_preparation(&self, _: Step2Open) -> Result<Box<dyn Step2Prep>, Step2Refusal> {
        self.preparations.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(HeldPrep {
            _controller: self.lock()?,
        }))
    }
    async fn reopen_for_resend(
        &self,
        _: Arc<dyn SplitConnect>,
        directory: PathBuf,
        target_cube: String,
        digest: sha256::Hash,
    ) -> Result<Box<dyn Step2Coord>, Step2Refusal> {
        assert_eq!(
            (directory, target_cube, digest),
            (self.directory.clone(), TARGET.to_string(), self.digest)
        );
        if let Some(refusal) = self.refuse_uncertain.lock().unwrap().clone() {
            return Err(refusal);
        }
        self.uncertain.fetch_add(1, Ordering::SeqCst);
        let lock = self.lock()?;
        Ok(Box::new(HeldCoord { _controller: lock }))
    }
}
impl ReconPort for Port {
    fn context(&self) -> Context {
        context()
    }
    fn open_reconciler(
        &self,
        _: PathBuf,
        _: String,
        _: sha256::Hash,
    ) -> Result<Box<dyn Step2Recon>, Step2Refusal> {
        self.reconcilers.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(HeldRecon {
            _controller: self.lock()?,
        }))
    }
}
fn port(journal: &Journal) -> Arc<Port> {
    port_as(journal, "synthetic-account")
}
/// [`port`] for the session of `account`.
fn port_as(journal: &Journal, account: &'static str) -> Arc<Port> {
    Arc::new(Port {
        directory: journal.temp.0.clone(),
        digest: journal.digest(),
        preparations: AtomicUsize::new(0),
        reconcilers: AtomicUsize::new(0),
        uncertain: AtomicUsize::new(0),
        refuse_uncertain: std::sync::Mutex::new(None),
        account,
    })
}

/// The step-1 side reopened by `leave_for_step1`: opens the real journal.
struct Connect {
    directory: PathBuf,
    digest: sha256::Hash,
    dropped: Arc<AtomicUsize>,
}
#[async_trait]
impl SplitConnect for Connect {
    fn context(&self) -> Context {
        context()
    }
    fn evidence(&self) -> &dyn SplitEvidenceSource {
        unreachable!("the ordering tests read no chain")
    }
    async fn window(&self) -> Result<ForkWindow, String> {
        unreachable!()
    }
    async fn bitcoin_feerate(&self) -> Option<u64> {
        unreachable!()
    }
    async fn address_used(&self, _: ChainId, _: &str) -> Result<bool, FailureKind> {
        unreachable!()
    }
    fn open(&self, _: OpenRequest) -> Result<Box<dyn Step1Driver>, CoordinatorError> {
        let controller = Controller::reopen_settling_blocking(
            &self.directory,
            &claim_workflow::split_identity(TARGET.into(), self.digest),
            context(),
        )?;
        Ok(Box::new(HeldStep1 {
            _controller: controller,
            dropped: self.dropped.clone(),
        }))
    }
}

/// #626 ordering: the step-1 driver (holding the journal lock) is dropped
/// before the preparation opens, so the open takes the lock at once instead
/// of waiting 2 s and refusing as busy. Leaving for a reorg review drops the
/// preparation before the step-1 driver reopens, the same way.
#[tokio::test(flavor = "multi_thread")]
async fn step2_entry_and_exit_release_the_journal_lock_first() {
    let journal = Journal::new(false);
    let port = port(&journal);
    let dropped = Arc::new(AtomicUsize::new(0));
    let step1_driver = Box::new(HeldStep1 {
        _controller: journal.lock(),
        dropped: dropped.clone(),
    });
    let started = std::time::Instant::now();
    let prep = enter_step2(step1_driver, port.clone(), journal.open())
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_millis(1_500));
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
    assert_eq!(port.preparations.load(Ordering::SeqCst), 1);
    // While the preparation holds it, nothing else opens the journal.
    assert!(tokio::task::spawn_blocking({
        let port = port.clone();
        move || port.lock().map(|_| ())
    })
    .await
    .unwrap()
    .is_err());

    let connect = Arc::new(Connect {
        directory: journal.temp.0.clone(),
        digest: journal.digest(),
        dropped: dropped.clone(),
    });
    let started = std::time::Instant::now();
    let reopened = leave_for_step1(
        prep,
        connect,
        OpenRequest {
            directory: journal.temp.0.clone(),
            target_cube: TARGET.into(),
            construction: journal.step1.clone(),
            verified: sign1(&journal.step1, &journal.wallet),
            fork_height: FORK,
            resume: true,
        },
    )
    .await
    .unwrap();
    assert!(started.elapsed() < Duration::from_millis(1_500));
    drop(reopened);
    assert_eq!(dropped.load(Ordering::SeqCst), 2);
}

/// Restart: a journal with no recorded step-2 submission resumes step 1 (the
/// reconciler is not opened); one with a recorded step-2 submission opens
/// only the reconciler, without rebuilding step 1, and the decision's own
/// journal read has released the lock by then. The decision reads the
/// journal under the session's context and needs no step-2 port: without a
/// reconciler a recorded step 2 refuses rather than fall back to step 1, and
/// a step-1 journal still resumes (#637 R1). A step-2 port does not change
/// that while the journal allows no resend (P3-3).
#[tokio::test(flavor = "multi_thread")]
async fn restart_reconciles_only_after_a_recorded_step2_submission() {
    let plain = Journal::new(false);
    let port_plain = port(&plain);
    for recon in [Some(port_plain.clone() as Arc<dyn ReconPort>), None] {
        assert!(matches!(
            restart(
                context(),
                recon,
                Some(resend_ports(&plain, &port_plain)),
                plain.temp.0.clone(),
                TARGET.into(),
                plain.digest()
            )
            .await,
            Ok(Restart::Step1)
        ));
    }
    assert_eq!(port_plain.reconcilers.load(Ordering::SeqCst), 0);
    assert_eq!(port_plain.uncertain.load(Ordering::SeqCst), 0);

    let submitted = Journal::new(true);
    let port_submitted = port(&submitted);
    let started = std::time::Instant::now();
    assert!(matches!(
        restart(
            context(),
            Some(port_submitted.clone()),
            Some(resend_ports(&submitted, &port_submitted)),
            submitted.temp.0.clone(),
            TARGET.into(),
            submitted.digest()
        )
        .await,
        // Nothing recorded its return: the dead end comes with it (#625 F2),
        // and no resend is mentioned.
        Ok(Restart::Reconcile(_, Some(_), None))
    ));
    assert!(started.elapsed() < Duration::from_millis(1_500));
    assert_eq!(port_submitted.reconcilers.load(Ordering::SeqCst), 1);
    assert_eq!(port_submitted.preparations.load(Ordering::SeqCst), 0);
    assert_eq!(port_submitted.uncertain.load(Ordering::SeqCst), 0);
    // No reconciler for the session: refused, retryable, and nothing else
    // is opened; the journal is not left locked.
    match restart(
        context(),
        None,
        None,
        submitted.temp.0.clone(),
        TARGET.into(),
        submitted.digest(),
    )
    .await
    {
        Err(refusal) => {
            assert!(refusal.retry);
            assert_eq!(refusal.reason, RECONCILE_UNAVAILABLE);
        }
        Ok(_) => panic!("a recorded step 2 restarted without a reconciler"),
    }
    assert_eq!(port_submitted.reconcilers.load(Ordering::SeqCst), 1);
    assert_eq!(port_submitted.preparations.load(Ordering::SeqCst), 0);
    drop(submitted.lock());
    // Another account's journal is not read as either.
    let mut other = context();
    other.account = "other".into();
    assert!(restart(
        other,
        Some(port_submitted.clone()),
        None,
        submitted.temp.0.clone(),
        TARGET.into(),
        submitted.digest()
    )
    .await
    .is_err());
    assert_eq!(port_submitted.reconcilers.load(Ordering::SeqCst), 1);
}

/// The step-2 port and step-1 session a restart may reopen a resend with.
fn resend_ports(
    journal: &Journal,
    port: &Arc<Port>,
) -> (Arc<dyn Step2Port>, Arc<dyn SplitConnect>) {
    (
        port.clone(),
        Arc::new(Connect {
            directory: journal.temp.0.clone(),
            digest: journal.digest(),
            dropped: Arc::new(AtomicUsize::new(0)),
        }),
    )
}

/// P3-3: a restart reopens the submission coordinator only for a resend the
/// journal allows (the latest attempt recorded as returned unaccepted, the
/// step 2 never seen on BTCB2) and only through the same session's step-2
/// port, after its own journal read released the lock. Otherwise, or when
/// that reopen refuses, the reconciler is opened as before, with the reason
/// a resend is unavailable; nothing is left holding the journal.
#[tokio::test(flavor = "multi_thread")]
async fn restart_reopens_the_coordinator_only_for_a_resend_the_journal_allows() {
    async fn run(
        journal: &Journal,
        port: &Arc<Port>,
        resend: bool,
    ) -> Result<Restart, Step2Refusal> {
        restart(
            context(),
            Some(port.clone()),
            resend.then(|| resend_ports(journal, port)),
            journal.temp.0.clone(),
            TARGET.into(),
            journal.digest(),
        )
        .await
    }

    let returned = Journal::returned(false);
    let ports = port(&returned);
    let started = std::time::Instant::now();
    let reopened = run(&returned, &ports, true).await;
    assert!(started.elapsed() < Duration::from_millis(1_500));
    assert!(matches!(reopened, Ok(Restart::Resend(_))));
    assert_eq!(ports.uncertain.load(Ordering::SeqCst), 1);
    assert_eq!(ports.reconcilers.load(Ordering::SeqCst), 0);
    // The reopened coordinator holds the journal; dropping it releases it.
    assert!(tokio::task::spawn_blocking({
        let ports = ports.clone();
        move || ports.lock().map(|_| ())
    })
    .await
    .unwrap()
    .is_err());
    drop(reopened);
    drop(ports.lock().unwrap());

    // No step-2 port (the Vault daemon unloaded or on a route step 2 can't
    // be sent through): the reconciler, saying why.
    match run(&returned, &ports, false).await {
        Ok(Restart::Reconcile(_, None, Some(note))) => assert_eq!(note, RESEND_NEEDS_VAULT),
        _ => panic!("no step-2 port"),
    }
    // A step-2 port of another session is not used.
    let other = port_as(&returned, "other-account");
    match run(&returned, &other, true).await {
        Ok(Restart::Reconcile(_, None, Some(note))) => assert_eq!(note, RESEND_NEEDS_VAULT),
        _ => panic!("another session's port"),
    }
    assert_eq!(other.uncertain.load(Ordering::SeqCst), 0);
    // The reopen refuses (Connect, a rebuild that does not verify, a route
    // no longer admitted): the reconciler, with that reason.
    *ports.refuse_uncertain.lock().unwrap() = Some(Step2Refusal::retry(
        "Connect couldn't read Bitcoin Blake2b's status.",
    ));
    match run(&returned, &ports, true).await {
        Ok(Restart::Reconcile(_, None, Some(note))) => {
            assert!(note.contains("can't be sent again right now"), "{}", note);
            assert!(note.contains("Connect couldn't read"), "{}", note);
        }
        _ => panic!("a refused reopen"),
    }
    assert_eq!(ports.uncertain.load(Ordering::SeqCst), 1);
    assert_eq!(ports.reconcilers.load(Ordering::SeqCst), 2);
    drop(ports.lock().unwrap());
    // Restoring step 1 finds a claimed coin spent on BTCB2: final, and this
    // step 2 may itself be the spender, so the note doesn't blame anything
    // else (#648 R1). Another final refusal isn't "right now" either.
    let spent = step1::evidence_refusal(crate::services::split_evidence::EvidenceError {
        outpoint: Some(OutPoint::new(Txid::from_byte_array([1; 32]), 0)),
        failure: crate::services::split_evidence::EvidenceFailure::Btcb2Spent,
    });
    assert!(!spent.retry);
    *ports.refuse_uncertain.lock().unwrap() = Some(Step2Refusal::final_(spent.reason));
    match run(&returned, &ports, true).await {
        Ok(Restart::Reconcile(_, None, Some(note))) => assert_eq!(note, RESEND_COIN_SPENT),
        _ => panic!("a spent claimed coin"),
    }
    *ports.refuse_uncertain.lock().unwrap() =
        Some(Step2Refusal::final_("This split can't be resumed here."));
    match run(&returned, &ports, true).await {
        Ok(Restart::Reconcile(_, None, Some(note))) => {
            assert!(note.starts_with("Step 2 can't be sent again: "), "{}", note);
            assert!(note.contains("can't be resumed here"), "{}", note);
        }
        _ => panic!("a final refusal"),
    }
    assert_eq!(ports.uncertain.load(Ordering::SeqCst), 1);
    assert_eq!(ports.reconcilers.load(Ordering::SeqCst), 4);
    drop(ports.lock().unwrap());
    *ports.refuse_uncertain.lock().unwrap() = None;

    // At the resend limit the restart opens only the reconciler, in its dead
    // end, and no resend is mentioned (#648 R3a).
    let exhausted = Journal::at_resend_limit();
    let ports = port(&exhausted);
    assert!(matches!(
        run(&exhausted, &ports, true).await,
        Ok(Restart::Reconcile(_, Some(_), None))
    ));
    assert_eq!(ports.uncertain.load(Ordering::SeqCst), 0);
    assert_eq!(ports.reconcilers.load(Ordering::SeqCst), 1);

    // A step 2 ever seen on BTCB2 is never resent: the reconciler, and no
    // resend is mentioned.
    let observed = Journal::returned(true);
    let ports = port(&observed);
    assert!(matches!(
        run(&observed, &ports, true).await,
        Ok(Restart::Reconcile(_, None, None))
    ));
    assert_eq!(ports.uncertain.load(Ordering::SeqCst), 0);
    assert_eq!(ports.reconcilers.load(Ordering::SeqCst), 1);
}

/// An authenticated Connect session at a synthetic origin. Nothing here
/// sends a request.
fn session(origin: &str) -> ConnectSession {
    let mut client = crate::services::coincube::CoincubeClient::for_test(origin);
    client.set_token("synthetic-test-token");
    ConnectSession {
        client,
        account: "synthetic-account".into(),
    }
}
const ORIGIN: &str = "https://connect.example/";

/// A target Vault daemon's configuration on `chain` with `backend`.
fn vault_config(
    chain: ChainId,
    backend: Option<coincubed::config::BitcoinBackend>,
) -> coincubed::config::Config {
    coincubed::config::Config::new(
        coincubed::config::BitcoinConfig::new(chain, Duration::from_secs(30)),
        backend,
        log::LevelFilter::Off,
        CoincubeDescriptor::from_str(
            "wsh(or_d(pk([f5acc2fd]tpubD6NzVbkrYhZ4YgUx2ZLNt2rLYAMTdYysCRzKoLu2BeSHKvzqPaBDvf17GeBPnExUVPkuBpx4kniP964e2MxyzzazcXLptxLXModSVCVEV1T/<0;1>/*),and_v(v:pkh([8a64f2a9]tpubD6NzVbkrYhZ4WmzFjvQrp7sDa4ECUxTi9oby8K4FZkd3XCBtEdKwUiQyYJaxiJo5y42gyDWEczrFpozEjeLxMPxjf2WtkfcbpUdfvNnozWF/<0;1>/*),older(10))))#d72le4dr",
        )
        .unwrap(),
        coincubed::datadir::DataDirectory::new(PathBuf::from("/synthetic-unused-split-panel")),
    )
}
fn embedded(config: coincubed::config::Config) -> Arc<dyn Daemon + Send + Sync> {
    Arc::new(crate::daemon::embedded::EmbeddedDaemon::unstarted_for_test(
        config, None,
    ))
}

/// An external daemon's RPC, which admission must never call.
#[derive(Debug)]
struct NoRpc;
impl crate::daemon::client::Client for NoRpc {
    type Error = crate::daemon::DaemonError;
    fn request<
        S: serde::Serialize + std::fmt::Debug,
        D: serde::de::DeserializeOwned + std::fmt::Debug,
    >(
        &self,
        method: &str,
        _: Option<S>,
    ) -> Result<D, Self::Error> {
        unreachable!("admission called the daemon: {}", method)
    }
}

/// #637 R2: the step-2 port exists only for a Vault daemon on a route step 2
/// can be sent through, by the same admission `finish` applies again at the
/// handoff: embedded, on BTCB2 mainnet, and on exactly Connect's BTCB2
/// Esplora at the session's origin (no token, no fallback) or a bound
/// Bitcoind node (P4). Anything else is refused before anything is
/// reserved, built or signed. The session's reconcile-only port takes no
/// daemon at all (#637 R1).
#[test]
fn step2_port_admits_only_a_route_step2_can_be_sent_through() {
    use coincubed::config::{
        BitcoinBackend, BitcoindConfig, BitcoindRpcAuth, ElectrumConfig, EsploraConfig,
    };
    let endpoint = "https://connect.example/api/v1/esplora/bitcoin-blake2b/mainnet";
    let esplora = |addr: &str| EsploraConfig {
        addr: addr.to_owned(),
        token: None,
        fallback_addr: None,
        fallback_token: None,
        secondary_fallback_addr: None,
        secondary_fallback_token: None,
    };
    let connect = || Some(BitcoinBackend::Esplora(esplora(endpoint)));
    let node = || {
        Some(BitcoinBackend::Bitcoind(BitcoindConfig {
            addr: "127.0.0.1:8332".parse().unwrap(),
            rpc_auth: BitcoindRpcAuth::CookieFile("/synthetic/.cookie".into()),
        }))
    };
    let (_sender, generation) = watch::channel(7);
    let port =
        |origin: &str, daemon| ProductionStep2::new(session(origin), generation.clone(), daemon);

    for (route, backend) in [("connect", connect()), ("node", node())] {
        let daemon = embedded(vault_config(ChainId::BitcoinBlake2b, backend));
        let admitted =
            port(ORIGIN, daemon.clone()).unwrap_or_else(|e| panic!("{}: {:?}", route, e));
        assert_eq!(
            admitted.identity().daemon,
            Arc::as_ptr(&daemon) as *const () as usize
        );
    }

    let with = |edit: fn(&mut EsploraConfig)| {
        let mut config = esplora(endpoint);
        edit(&mut config);
        Some(BitcoinBackend::Esplora(config))
    };
    let mut fallback = vault_config(ChainId::BitcoinBlake2b, connect());
    fallback.fallback_esplora = Some(esplora("https://mempool.example/api"));
    let refused: Vec<(&str, Arc<dyn Daemon + Send + Sync>)> = vec![
        (
            "electrum",
            embedded(vault_config(
                ChainId::BitcoinBlake2b,
                Some(BitcoinBackend::Electrum(ElectrumConfig {
                    addr: "ssl://electrum.example:50002".into(),
                    validate_domain: true,
                })),
            )),
        ),
        (
            "another esplora",
            embedded(vault_config(
                ChainId::BitcoinBlake2b,
                Some(BitcoinBackend::Esplora(esplora(
                    "https://mempool.example/api",
                ))),
            )),
        ),
        (
            "connect's bitcoin esplora",
            embedded(vault_config(
                ChainId::BitcoinBlake2b,
                Some(BitcoinBackend::Esplora(esplora(
                    "https://connect.example/api/v1/esplora/bitcoin/mainnet",
                ))),
            )),
        ),
        (
            "token",
            embedded(vault_config(
                ChainId::BitcoinBlake2b,
                with(|c| c.token = Some("synthetic".into())),
            )),
        ),
        (
            "esplora fallback",
            embedded(vault_config(
                ChainId::BitcoinBlake2b,
                with(|c| c.fallback_addr = Some("https://other.example/api".into())),
            )),
        ),
        ("vault fallback", embedded(fallback)),
        (
            "no backend",
            embedded(vault_config(ChainId::BitcoinBlake2b, None)),
        ),
        (
            "bitcoin",
            embedded(vault_config(ChainId::Bitcoin, connect())),
        ),
        (
            "btcb2 testnet4",
            embedded(vault_config(ChainId::BitcoinBlake2bTestnet4, connect())),
        ),
        (
            "external daemon",
            Arc::new(crate::daemon::client::Coincubed::new(NoRpc)),
        ),
    ];
    for (case, daemon) in refused {
        assert!(port(ORIGIN, daemon).is_err(), "{}", case);
    }
    // Connect's Esplora at another origin than the session's.
    let daemon = embedded(vault_config(ChainId::BitcoinBlake2b, connect()));
    assert!(port("https://other.example/", daemon).is_err());

    // The reconcile-only port: the session alone.
    let recon = ProductionRecon::new(session(ORIGIN), generation.clone(), None).unwrap();
    assert_eq!(recon.context().account, "synthetic-account");
    assert_eq!(recon.context().generation, 7);
}

/// The user copy: every target and construction refusal has text; a used
/// address names the chain and offers a new reservation; an unavailable read
/// says it is not evidence of use; the node route carries the privacy note
/// and Connect none; the review's switched-backend refusal says nothing was
/// sent.
#[test]
fn step2_copy_names_the_cause_and_the_node_route_privacy() {
    let used = describe_target(TargetError::Used(ChainId::Bitcoin));
    assert!(used.retry && used.reason.contains("on Bitcoin") && used.reason.contains("new one"));
    let used = describe_target(TargetError::Used(ChainId::BitcoinBlake2b));
    assert!(used.reason.contains("Bitcoin Blake2b"));
    let unavailable = describe_target(TargetError::Unavailable(
        ChainId::BitcoinBlake2b,
        FailureKind::Http(503),
    ));
    assert!(unavailable.retry && unavailable.reason.contains("not a sign"));
    assert!(!describe_target(TargetError::NotTargetVault).retry);
    for error in [
        TargetError::AlreadyReserved,
        TargetError::NotTracking,
        TargetError::NoReservation,
        TargetError::ReservationUnavailable,
    ] {
        let refusal = describe_target(error);
        assert!(refusal.retry && !refusal.reason.is_empty());
    }
    assert!(describe_step2(Step2Error::FeeUnavailable)
        .reason
        .contains("fee estimate"));
    assert!(describe_step2(Step2Error::NotChecked).retry);
    assert!(!describe_step2(Step2Error::DescriptorsForgotten).retry);
    let depth = describe_split_check(SplitCheckError::Coordinator(CoordinatorError::NotReady(
        Assessment::WaitingForDepth { confirmations: 4 },
    )));
    assert!(depth.reason.contains("4 of 6"));
    let switched = describe_check(CoordinatorError::Preflight(
        crate::services::claim_preflight::Error::BackendChanged,
    ));
    assert!(switched.retry && switched.reason.contains("nothing was sent"));
    assert!(RESERVING.contains("fresh receive address"));
    // A recorded submission may never have reached the transport
    // (`Outcome::Uncertain`): the reorg warning doesn't claim step 2 was
    // sent, and keeps the replay-protection and no-recovery guidance
    // (#637 Copilot review 5401909718).
    let reorged = reconcile_warning(Step1AfterStep2::Missing).unwrap();
    assert_eq!(reorged, STEP1_REORGED_AFTER_STEP2);
    for wanted in [
        "a submission of step 2 was recorded; it was sent or may have been sent",
        "replay protection for step 2 is no longer established",
        "no recovery",
        "not sent automatically",
    ] {
        assert!(reorged.contains(wanted), "{}", wanted);
    }
    assert!(!reorged.contains("step 2 was sent:"), "{}", reorged);
    assert!(!reorged.contains("sent again"), "{}", reorged);

    assert_eq!(route_copy(SubmissionRoute::Connect), ("Connect", None));
    let node = SubmissionRoute::BitcoinNode {
        address: "127.0.0.1:8332".parse().unwrap(),
        identity: crate::services::claim_coordinator::NodeIdentity::for_test(1),
    };
    assert_eq!(route_copy(node), ("Your Bitcoin node", Some(NODE_PRIVACY)));
    assert!(NODE_PRIVACY.contains("network address"));
}

/// P3-3 copy. Every resend refusal says nothing was sent. `Unsettled` says
/// this version can't resend and names abandon or reset as the way out, as
/// does the attempt limit; a sighting is final; a spent claimed coin names
/// it; an unavailable read is retryable and not called a spend; the
/// coordinator's own refusals read as for step 2.
#[test]
fn step2_resend_copy_names_the_way_out() {
    let spent = OutPoint::new(Txid::from_byte_array([1; 32]), 3);
    let refusals = [
        describe_resend(ResendError::NotRecorded),
        describe_resend(ResendError::Observed),
        describe_resend(ResendError::Unsettled),
        describe_resend(ResendError::AttemptsExhausted),
        describe_resend(ResendError::ClaimedCoinSpent(spent)),
        describe_resend(ResendError::Unavailable(spent, FailureKind::Http(503))),
    ];
    for refusal in &refusals {
        assert!(
            refusal.reason.contains("Nothing was sent"),
            "{}",
            refusal.reason
        );
    }
    let [not_recorded, observed, unsettled, exhausted, coin_spent, unavailable] = refusals;
    assert!(!not_recorded.retry && not_recorded.reason.contains("nothing to send again"));
    assert!(!observed.retry && observed.reason.contains("never sent again"));
    assert_eq!(unsettled.reason, RESEND_UNSETTLED);
    assert!(!unsettled.retry);
    for wanted in ["can't send step 2 again", "abandon or reset this split"] {
        assert!(unsettled.reason.contains(wanted), "{}", wanted);
    }
    assert!(!exhausted.retry);
    assert!(exhausted.reason.contains(&format!(
        "{} times",
        claim_workflow::MAX_SPLIT_STEP2_RESUBMISSIONS
    )));
    assert!(exhausted.reason.contains("abandon or reset this split"));
    assert!(!coin_spent.retry && coin_spent.reason.contains(&spent.to_string()));
    assert!(unavailable.retry && unavailable.reason.contains("not a sign a coin was spent"));
    let expired = describe_resend(ResendError::Coordinator(CoordinatorError::ExpiredEvidence));
    assert_eq!(
        expired,
        describe_step2(Step2Error::Coordinator(CoordinatorError::ExpiredEvidence))
    );
    assert!(expired.retry);
    assert!(!describe_resend(ResendError::Coordinator(CoordinatorError::Unsupported)).retry);
    assert!(RESEND_NEEDS_VAULT.contains("status can still be checked"));
}

/// P3-3: a resend review on screen lapses with its deadline, the
/// coordinator's revocation, a generation change or the generation's sender
/// going away, whichever comes first.
#[test]
fn resend_review_lapses_with_its_deadline_revocation_and_generation() {
    let far = Instant::now() + Duration::from_secs(60);
    let (sender, generation) = watch::channel(7);
    let revoked = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let live = {
        let revoked = revoked.clone();
        resend_liveness(
            move || revoked.load(Ordering::SeqCst),
            generation.clone(),
            7,
            far,
        )
    };
    assert!(live());
    revoked.store(true, Ordering::SeqCst);
    assert!(!live());
    revoked.store(false, Ordering::SeqCst);
    assert!(live());
    sender.send(8).unwrap();
    assert!(!live());
    sender.send(7).unwrap();
    assert!(live());
    drop(sender);
    assert!(!live());

    let (_sender, generation) = watch::channel(7);
    let past = resend_liveness(|| false, generation.clone(), 7, Instant::now());
    assert!(!past());
    let other = resend_liveness(|| false, generation, 6, far);
    assert!(!other());
}

/// "Split — cannot replay" only from live evidence (#636 P3-2): a token from
/// a successful check produces it and it names that check's tracked txid.
/// It disappears with the evidence, not only by time: when a later check
/// supersedes it, on revocation (logout, Cube close), on a generation change
/// and when the preparation is gone.
#[test]
fn cannot_replay_label_comes_only_from_live_check_evidence() {
    let prevouts = [OutPoint::new(Txid::from_byte_array([1; 32]), 0)];
    let tracked = Txid::from_byte_array([2; 32]);
    let (sender, generation) = watch::channel(7);
    // Superseded by a later check.
    let (token, latest) =
        ForeignStep2Authorization::for_test(&prevouts, tracked, generation.clone());
    let label = evidence_of(&token);
    assert_eq!(label.label(), Some(CANNOT_REPLAY));
    assert_eq!(label.tracked_txid(), tracked);
    latest.store(2, Ordering::Release);
    assert_eq!(label.label(), None);
    // The preparation (its check counter) dropped.
    let (token, latest) =
        ForeignStep2Authorization::for_test(&prevouts, tracked, generation.clone());
    let label = evidence_of(&token);
    drop(latest);
    assert_eq!(label.label(), None);
    // A generation change (session or account change).
    let (token, _latest) =
        ForeignStep2Authorization::for_test(&prevouts, tracked, generation.clone());
    let label = evidence_of(&token);
    assert_eq!(label.label(), Some(CANNOT_REPLAY));
    sender.send(8).unwrap();
    assert_eq!(label.label(), None);
    // A closed session (the generation's sender gone) is not live either.
    let (token, _latest) =
        ForeignStep2Authorization::for_test(&prevouts, tracked, watch::channel(7).1);
    assert_eq!(evidence_of(&token).label(), None);
    // Revocation (logout, Cube close, backend switch).
    let (token, _latest) =
        ForeignStep2Authorization::for_test(&prevouts, tracked, generation.clone());
    let label = evidence_of(&token);
    assert_eq!(label.label(), Some(CANNOT_REPLAY));
    token.revoke_for_test();
    assert_eq!(label.label(), None);
}

/// D1: the step-2 panel layer is reached only through the Split panel
/// (itself constructed in production only to resume an existing journal,
/// `split_panel_has_no_gui_entry_point`), and the App only hands the panel
/// its port. Whole identifiers.
#[test]
fn step2_panel_layer_is_reached_only_through_the_split_panel() {
    fn walk(dir: &Path, files: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push((
                    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    std::fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut unexpected = Vec::new();
    for (file, text) in &files {
        if file.starts_with("src/app/state/vault/split/") {
            continue;
        }
        for ident in [
            "ProductionStep2",
            "Step2Port",
            "Step2Prep",
            "Step2Coord",
            "Step2Recon",
            "Step2Open",
            "Restart",
            "restart",
            "enter_step2",
            "leave_for_step1",
            "CannotReplay",
            "Step2Refusal",
            "Step2ReviewView",
            "describe_target",
            "describe_step2",
            "describe_split_check",
            "route_copy",
            "NODE_PRIVACY",
            "CANNOT_REPLAY",
            "RESERVING",
            "FinishRefusal",
            "set_step2_port",
            "PortIdentity",
            "ProductionRecon",
            "ReconPort",
            "set_recon_port",
            "RECONCILE_UNAVAILABLE",
            "STEP1_REORGED_AFTER_STEP2",
            "reconcile_warning",
            // #642 (#637 review E1 and E3).
            "Step2Recovery",
            // P3-3.
            "reopen_for_resend",
            "review_resend",
            "confirm_resend",
            "Step2ResendView",
            "ResendLiveness",
            "describe_resend",
            "RESEND_NEEDS_VAULT",
            "RESEND_COIN_SPENT",
            "RESEND_UNSETTLED",
            // #625 F2: closing a step-2 dead end.
            "DeadEnd",
            "check_close",
            // S3-D4: why there is no step-2 port.
            "Step2Unavailable",
            "unavailable_copy",
            "STEP2_NEEDS_VAULT",
            "STEP2_UNSUPPORTED_ROUTE",
            "STEP2_REFUSED",
            // #568 B5b: the completion stage.
            "SplitCompletion",
            "CompletionSite",
            "CompletionStanding",
            "complete_from_coordinator",
            "completion_stands",
            "RevokeSlot",
            "SPLIT_COMPLETED",
            "COMPLETION_NOT_YET",
            "COMPLETION_EXPIRED",
            "COMPLETION_NOT_RECORDED",
            "COMPLETION_NOT_FORGOTTEN",
            "COMPLETION_NO_VAULT",
            "COMPLETION_INTERRUPTED",
            "COMPLETION_LOST",
            // #568 S4b: the O1 acknowledgement and the O4 exit.
            "ReconfirmationView",
            "describe_reconfirmation",
            "rdts_reconfirmation_refusal",
            "RECONFIRMATION_AGAIN",
            "conflict_close_copy",
            "conflict_closable_copy",
            "CONFLICT_STEP1_SEEN",
            // #568 B5c-1: the copy sweep.
            "COMPLETION_CHECK_EXPIRED",
            "COMPLETION_RECORD_UNAVAILABLE",
            "describe_completion",
            "conflict_coin_unspent_copy",
            "set_split_from",
        ] {
            let named = text
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|word| word == ident);
            // The App hands the panel the port of its Vault daemon and the
            // session's reconcile-only port with its completion site; the
            // view shows the N4 waiting text and the completion copy.
            let allowed = (file == "src/app/mod.rs"
                && [
                    "ProductionStep2",
                    "Step2Port",
                    "set_step2_port",
                    "ProductionRecon",
                    "ReconPort",
                    "set_recon_port",
                    // S3-D4: the port build says why there is no step 2.
                    "Step2Unavailable",
                    // B5b: where the reconcile-only port records a
                    // completion.
                    "CompletionSite",
                    // B5c-1: the Cube's completion records, for a restart
                    // into Completed.
                    "set_split_from",
                ]
                .contains(&ident))
                || (file == "src/app/view/vault/split.rs"
                    && [
                        "RESERVING",
                        "SPLIT_COMPLETED",
                        // #568 S4b: O4's close copy.
                        "conflict_close_copy",
                        "conflict_closable_copy",
                    ]
                    .contains(&ident));
            // `restart`/`Restart` are common words elsewhere: only a path
            // into the step-2 module counts for them.
            let generic = ["restart", "Restart"].contains(&ident)
                && !text.contains(&format!("step2::{ident}"));
            if named && !allowed && !generic {
                unexpected.push((file.clone(), ident));
            }
        }
    }
    assert!(unexpected.is_empty(), "{:?}", unexpected);
}

/// Legolas #652 P-B: the resend refusal on a recorded step-1 conflict has
/// its own copy naming the conflict, final either way: a terminal one says
/// step 1 can never confirm; a provisional one that the spend may still be
/// unconfirmed (S4-D5). Neither is another state's message.
#[test]
fn step2_resend_refusal_on_a_step1_conflict_names_it_and_is_final() {
    use crate::services::claim_workflow::Step1Conflict;
    use coincube_core::claim::BlockRef;
    let block = |height: u64, n: u8| BlockRef {
        height,
        hash: coincube_core::miniscript::bitcoin::BlockHash::from_byte_array([n; 32]),
    };
    let spent = OutPoint::new(Txid::from_byte_array([7; 32]), 1);
    let provisional = Step1Conflict::new(spent, block(120, 4));
    let terminal = provisional.terminal(block(126, 5)).unwrap();
    for (conflict, wanted) in [
        (
            terminal,
            "so step 1 can never confirm and this split can't complete",
        ),
        (provisional, "(it may still be unconfirmed)"),
    ] {
        let refusal = describe_resend(ResendError::Step1ConflictRecorded(conflict));
        assert!(!refusal.retry, "{}", refusal.reason);
        assert_eq!(refusal.recovery, Step2Recovery::None);
        assert!(
            !refusal.reason.contains("already recorded on this device"),
            "{}",
            refusal.reason
        );
        assert!(refusal.reason.contains(&spent.to_string()));
        assert!(refusal.reason.contains(wanted), "{}", refusal.reason);
        assert!(refusal.reason.contains("Nothing was sent"));
    }
}

/// #568 S4b, O1: the refusals of a review of step 1's new block. Past the
/// RDTS margin (S4-D4) or after RDTS expired, it is final and says the
/// split can't complete; a lapsed, changed or stale review is "review it
/// again"; anything else reads as for step 2. None says something was sent.
#[test]
fn step1_reconfirmation_refusal_copy_names_the_rdts_margin() {
    use crate::services::claim_coordinator::Error as E;
    let margin = describe_reconfirmation(E::NotReady(Assessment::ExpiryMargin));
    assert!(!margin.retry, "{:?}", margin);
    assert!(
        margin.reason.contains("expires within"),
        "{}",
        margin.reason
    );
    assert!(
        margin.reason.contains("can't complete"),
        "{}",
        margin.reason
    );
    let expired = describe_reconfirmation(E::NotReady(Assessment::RdtsExpired));
    assert!(!expired.retry, "{:?}", expired);
    assert!(expired.reason.contains("has expired"), "{}", expired.reason);
    for error in [E::ExpiredEvidence, E::ChangedReview, E::InvalidReview] {
        let again = describe_reconfirmation(error);
        assert!(again.retry, "{:?}", again);
        assert_eq!(again.reason, RECONFIRMATION_AGAIN);
    }
    let other = describe_reconfirmation(E::Revoked);
    assert_eq!(other, describe_step2(Step2Error::Coordinator(E::Revoked)));
    for refusal in [margin, expired, other] {
        assert!(!refusal.reason.contains("was sent."), "{}", refusal.reason);
    }
}

mod close;
mod copy;
mod driver;
mod panel;
