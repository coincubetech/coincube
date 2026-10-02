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
fn observe(controller: &mut Controller, txid: Txid, absent: bool) {
    let mut bundle = six_deep(txid);
    if absent {
        bundle.observations.bitcoin_transaction = TransactionObservation::Absent;
        bundle.observations.bitcoin.location = TransactionLocation::Unconfirmed;
    }
    let ticket = controller.begin_check(&context()).unwrap();
    controller
        .apply_observation(ticket, &context(), Ok(bundle), policy(), 10_000)
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
        let temp = Temp::new();
        let wallet = wallet();
        let step1 = step1(&wallet);
        let signed = sign1(&step1, &wallet);
        let tracked = signed.transaction().compute_txid();
        let mut c =
            Controller::create_split(&temp.0, TARGET.into(), &step1, &signed, FORK, context())
                .unwrap();
        observe(&mut c, tracked, true);
        c.record_split_broadcast_intent(&context(), &signed, policy(), 10_000)
            .unwrap();
        observe(&mut c, tracked, false);
        assert_eq!(c.phase(), Phase::Tracking);
        if with_step2 {
            c.record_split_target(&context(), 3, target_script())
                .unwrap();
            let construction = step2(&wallet, &step1);
            observe(&mut c, tracked, false);
            c.prepare_split_step2(&context(), &construction, policy(), 10_000)
                .unwrap();
            let secp = Secp256k1::new();
            let mut psbt = construction.psbt().clone();
            psbt.sign(&wallet.signer, &secp).unwrap();
            let verified =
                finalize_split_step2(&construction, &coins(&wallet), &wallet.source, &psbt, &secp)
                    .unwrap();
            observe(&mut c, tracked, false);
            c.record_split_step2_broadcast_intent(&context(), &verified, policy(), 10_000)
                .unwrap();
        }
        Self {
            temp,
            wallet,
            step1,
        }
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
    ) -> Result<(Status, TransactionObservation), Step2Refusal> {
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
impl Step2Port for Port {
    fn context(&self) -> Context {
        context()
    }
    fn open_preparation(&self, _: Step2Open) -> Result<Box<dyn Step2Prep>, Step2Refusal> {
        self.preparations.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(HeldPrep {
            _controller: self.lock()?,
        }))
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
    Arc::new(Port {
        directory: journal.temp.0.clone(),
        digest: journal.digest(),
        preparations: AtomicUsize::new(0),
        reconcilers: AtomicUsize::new(0),
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
/// journal read has released the lock by then.
#[tokio::test(flavor = "multi_thread")]
async fn restart_reconciles_only_after_a_recorded_step2_submission() {
    let plain = Journal::new(false);
    let port_plain = port(&plain);
    assert!(matches!(
        restart(
            port_plain.clone(),
            plain.temp.0.clone(),
            TARGET.into(),
            plain.digest()
        )
        .await,
        Ok(Restart::Step1)
    ));
    assert_eq!(port_plain.reconcilers.load(Ordering::SeqCst), 0);

    let submitted = Journal::new(true);
    let port_submitted = port(&submitted);
    let started = std::time::Instant::now();
    assert!(matches!(
        restart(
            port_submitted.clone(),
            submitted.temp.0.clone(),
            TARGET.into(),
            submitted.digest()
        )
        .await,
        Ok(Restart::Reconcile(_))
    ));
    assert!(started.elapsed() < Duration::from_millis(1_500));
    assert_eq!(port_submitted.reconcilers.load(Ordering::SeqCst), 1);
    assert_eq!(port_submitted.preparations.load(Ordering::SeqCst), 0);
    // Another account's journal is not read as either.
    let mut other = context();
    other.account = "other".into();
    struct OtherPort(Arc<Port>, Context);
    impl Step2Port for OtherPort {
        fn context(&self) -> Context {
            self.1.clone()
        }
        fn open_preparation(&self, open: Step2Open) -> Result<Box<dyn Step2Prep>, Step2Refusal> {
            self.0.open_preparation(open)
        }
        fn open_reconciler(
            &self,
            d: PathBuf,
            t: String,
            g: sha256::Hash,
        ) -> Result<Box<dyn Step2Recon>, Step2Refusal> {
            self.0.open_reconciler(d, t, g)
        }
    }
    assert!(restart(
        Arc::new(OtherPort(port_submitted.clone(), other)),
        submitted.temp.0.clone(),
        TARGET.into(),
        submitted.digest()
    )
    .await
    .is_err());
    assert_eq!(port_submitted.reconcilers.load(Ordering::SeqCst), 1);
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

    assert_eq!(route_copy(SubmissionRoute::Connect), ("Connect", None));
    let node = SubmissionRoute::BitcoinNode {
        address: "127.0.0.1:8332".parse().unwrap(),
        identity: crate::services::claim_coordinator::NodeIdentity::for_test(1),
    };
    assert_eq!(route_copy(node), ("Your Bitcoin node", Some(NODE_PRIVACY)));
    assert!(NODE_PRIVACY.contains("network address"));
}

/// "Split — cannot replay" only from live evidence: a token from a
/// successful check produces it, it names that check's tracked txid, and it
/// lapses with the check's evidence.
#[test]
fn cannot_replay_label_comes_only_from_live_check_evidence() {
    let prevouts = [OutPoint::new(Txid::from_byte_array([1; 32]), 0)];
    let tracked = Txid::from_byte_array([2; 32]);
    let (token, _live) =
        ForeignStep2Authorization::for_test(&prevouts, tracked, watch::channel(7).1);
    let label = evidence_of(&token);
    assert_eq!(label.label(), Some(CANNOT_REPLAY));
    assert_eq!(label.tracked_txid(), tracked);
    let lapsed = CannotReplay {
        tracked,
        until: Instant::now(),
    };
    assert_eq!(lapsed.label(), None);
}

/// D1: the step-2 panel layer has no caller yet. Its items are named only
/// in this module and its tests (B3b-2b-2 wires the panel, still reachable
/// only by resuming a journal).
#[test]
fn step2_panel_layer_has_no_caller_yet() {
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
        if file.starts_with("src/app/state/vault/split/step2") {
            continue;
        }
        for ident in [
            "ProductionStep2",
            "Step2Port",
            "Step2Prep",
            "Step2Coord",
            "Step2Recon",
            "enter_step2",
            "leave_for_step1",
            "CannotReplay",
            "Step2Refusal",
            "describe_target",
            "describe_step2",
            "route_copy",
            "NODE_PRIVACY",
            "FinishRefusal",
        ] {
            // Whole identifiers: `SplitStep2Coordinator` is not `Step2Coord`.
            let named = text
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|word| word == ident);
            if named {
                unexpected.push((file.clone(), ident));
            }
        }
    }
    assert!(unexpected.is_empty(), "{:?}", unexpected);
}
