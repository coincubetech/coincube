//! The single-step route in the Split panel (#568 B4b-3c) over fakes.
//!
//! The Connect side is `split::tests::FakeConnect` (synthetic chains; real
//! outpoint authentication). The unified port is a fake whose coordinator
//! core builds a real core unified sweep and verifies signatures with core's
//! unified finalizer, so the seeds below really sign: a 2-of-3 sortedmulti
//! of three synthetic BIP39 seeds. The panel's driver
//! ([`super::UnifiedDriver`]) is the production one. Journals are real
//! fork-only journals in a private temporary directory. No test prints a
//! seed, a passphrase or a derived key.

use std::{
    str::FromStr,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
};

use coincube_core::{
    bip39::Mnemonic,
    foreign_split::{
        create_unified_sweep, finalize_unified_sweep, UnifiedInputs, UnifiedSweep,
        VerifiedUnifiedSweep,
    },
    miniscript::bitcoin::{
        absolute::LockTime, bip32::DerivationPath, hashes::Hash, secp256k1::Secp256k1, ScriptBuf,
        WScriptHash,
    },
    signer::SessionSigner,
};

use super::*;
// Part 2's journal fixture signs with seeds it hands to `SeedSet::add`;
// `unified` no longer imports `Zeroizing` (its `SeedText` moved, D2).
use crate::{
    app::state::vault::split::{
        tests::{drive, FakeConnect, Scan, Temp, TARGET},
        SplitMessage,
    },
    services::{
        foreign_scan::{Branch, ScanDescriptor},
        split_test_wallets::{self as fixture},
    },
};
use zeroize::Zeroizing;

fn mnemonic(byte: u8) -> Mnemonic {
    Mnemonic::from_entropy(&[byte; 16]).unwrap()
}

/// A policy key's public material, as the seed set's signer derives it.
fn account(byte: u8, passphrase: &str, path: &str) -> String {
    let secp = Secp256k1::new();
    let signer =
        SessionSigner::from_mnemonic(Network::Bitcoin, mnemonic(byte), passphrase).unwrap();
    let origin = DerivationPath::from_str(path).unwrap();
    format!(
        "[{}/{}]{}",
        signer.fingerprint(&secp),
        path.trim_start_matches("m/"),
        signer.xpub_at(&origin, &secp)
    )
}

/// A 2-of-3 sortedmulti of seeds 1, 2 and 3 (seed 2 with a passphrase),
/// scanned on both chains with the fixture's two pre-fork coins.
fn seed_scan() -> Scan {
    let keys: Vec<_> = [(1, ""), (2, "second passphrase"), (3, "")]
        .iter()
        .map(|(byte, passphrase)| {
            format!("{}/{{b}}/*", account(*byte, passphrase, "m/48'/0'/0'/2'"))
        })
        .collect();
    let template = format!("wsh(sortedmulti(2,{}))", keys.join(","));
    let parse = |branch, step: u32| {
        ScanDescriptor::parse(branch, &template.replace("{b}", &step.to_string())).unwrap()
    };
    let wallet = fixture::Wallet {
        external: parse(Branch::External, 0),
        internal: parse(Branch::Internal, 1),
        signers: Vec::new(),
    };
    let coins = fixture::shared_coins(&wallet);
    Scan { wallet, coins }
}

fn words(byte: u8) -> SeedText {
    SeedText::from(mnemonic(byte).to_string())
}

/// The target Vault's address script stand-in (P2WSH).
fn target_script() -> ScriptBuf {
    ScriptBuf::new_p2wsh(&WScriptHash::from_byte_array([0x5a; 32]))
}

fn build_sweep(open: &UnifiedOpen, feerate: u64) -> UnifiedSweep {
    let target = target_script();
    let tip = fixture::BTCB2_TIP_HEIGHT;
    create_unified_sweep(
        &UnifiedInputs {
            chain: ChainId::BitcoinBlake2b,
            source: &open.source,
            coins: &open.coins,
            fork_height: open.fork_height,
            target: &target,
        },
        feerate,
        LockTime::from_height(tip).unwrap(),
        tip,
    )
    .unwrap()
}

/// What the panel asked of the fake port and its cores.
#[derive(Default)]
struct PortCalls {
    opens: Mutex<Vec<bool>>,
    recon_opens: AtomicUsize,
    reserves: AtomicUsize,
    builds: AtomicUsize,
    reviews: AtomicUsize,
    confirms: AtomicUsize,
    revoked: AtomicBool,
    /// The fee estimate the review reads (`None`: unavailable).
    review_feerate: Mutex<Option<u64>>,
    /// What a reconcile sees.
    seen: Mutex<Option<TransactionObservation>>,
    /// The route the review reports (default Connect).
    route: Mutex<Option<SubmissionRoute>>,
}

struct FakePort {
    context: Context,
    daemon: usize,
    calls: Arc<PortCalls>,
}
impl FakePort {
    fn new(context: Context, daemon: usize) -> Arc<Self> {
        let calls = Arc::new(PortCalls::default());
        *calls.review_feerate.lock().unwrap() = Some(2);
        Arc::new(Self {
            context,
            daemon,
            calls,
        })
    }
}
impl UnifiedPort for FakePort {
    fn context(&self) -> Context {
        self.context.clone()
    }
    fn identity(&self) -> PortIdentity {
        PortIdentity {
            context: self.context.clone(),
            daemon: self.daemon,
        }
    }
    fn open(&self, open: UnifiedOpen, resume: bool) -> Result<Box<dyn UnifiedFlow>, Step2Refusal> {
        self.calls.opens.lock().unwrap().push(resume);
        Ok(Box::new(UnifiedDriver::new(FakeCore {
            open,
            calls: self.calls.clone(),
            target: None,
            sweep: None,
            verified: None,
            reviewed: false,
        })))
    }
    fn open_reconciler(
        &self,
        _: PathBuf,
        _: String,
        _: sha256::Hash,
    ) -> Result<Box<dyn UnifiedRecon>, Step2Refusal> {
        self.calls.recon_opens.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(FakeRecon(self.calls.clone())))
    }
}

/// A coordinator core over a real core sweep.
struct FakeCore {
    open: UnifiedOpen,
    calls: Arc<PortCalls>,
    target: Option<u32>,
    sweep: Option<UnifiedSweep>,
    verified: Option<VerifiedUnifiedSweep>,
    reviewed: bool,
}
#[async_trait]
impl UnifiedCore for FakeCore {
    fn revoke_handle(&self) -> RevokeHandle {
        let calls = self.calls.clone();
        Arc::new(move || calls.revoked.store(true, Ordering::SeqCst))
    }
    fn target_index(&self) -> Option<u32> {
        self.target
    }
    fn recorded_outcome(&self) -> Option<Outcome> {
        None
    }
    async fn reserve(&mut self, _: &Context) -> Result<u32, TargetError> {
        self.calls.reserves.fetch_add(1, Ordering::SeqCst);
        self.target = Some(7);
        Ok(7)
    }
    async fn prove(&mut self, _: &Context) -> Result<(), TargetError> {
        Ok(())
    }
    async fn build(&mut self, _: &Context) -> Result<Psbt, UnifiedError> {
        self.calls.builds.fetch_add(1, Ordering::SeqCst);
        self.verified = None;
        let sweep = build_sweep(&self.open, 2);
        let psbt = sweep.psbt().clone();
        self.sweep = Some(sweep);
        Ok(psbt)
    }
    fn verify_signed(
        &mut self,
        _: &Context,
        signed: &UnifiedPsbt,
    ) -> Result<UnifiedReplayStatus, UnifiedError> {
        let sweep = self.sweep.as_ref().ok_or(UnifiedError::NotBuilt)?;
        let verified = finalize_unified_sweep(sweep, signed, &Secp256k1::verification_only())
            .map_err(UnifiedError::Finalize)?;
        let status = verified.replay_status();
        self.verified = Some(verified);
        Ok(status)
    }
    async fn review(&mut self, _: &Context) -> Result<ReviewFacts, UnifiedError> {
        self.calls.reviews.fetch_add(1, Ordering::SeqCst);
        let verified = self.verified.as_ref().ok_or(UnifiedError::NotSigned)?;
        self.reviewed = true;
        Ok(ReviewFacts {
            txid: verified.transaction().compute_txid(),
            fee_sats: verified.fee().to_sat(),
            vsize: verified.vsize(),
            route: self
                .calls
                .route
                .lock()
                .unwrap()
                .unwrap_or(SubmissionRoute::Connect),
            replay: verified.replay_status(),
        })
    }
    fn drop_review(&mut self) {
        self.reviewed = false;
    }
    async fn confirm(&mut self, _: &Context) -> Result<Outcome, UnifiedError> {
        self.calls.confirms.fetch_add(1, Ordering::SeqCst);
        if !std::mem::take(&mut self.reviewed) {
            return Err(UnifiedError::NotSigned);
        }
        let tx = self.verified.as_ref().unwrap().transaction();
        Ok(Outcome::Uncertain {
            txid: tx.compute_txid(),
            wtxid: tx.compute_wtxid(),
        })
    }
    async fn reconcile(&mut self, _: &Context) -> Result<TransactionObservation, UnifiedError> {
        Ok(self
            .calls
            .seen
            .lock()
            .unwrap()
            .unwrap_or(TransactionObservation::Absent))
    }
    async fn feerate(&self) -> Option<u64> {
        *self.calls.review_feerate.lock().unwrap()
    }
}

struct FakeRecon(Arc<PortCalls>);
#[async_trait]
impl UnifiedRecon for FakeRecon {
    fn revoke_handle(&self) -> RevokeHandle {
        let calls = self.0.clone();
        Arc::new(move || calls.revoked.store(true, Ordering::SeqCst))
    }
    fn recorded_outcome(&self) -> Option<Outcome> {
        None
    }
    async fn reconcile(&mut self, _: &Context) -> Result<TransactionObservation, Step2Refusal> {
        Ok(self
            .0
            .seen
            .lock()
            .unwrap()
            .unwrap_or(TransactionObservation::Absent))
    }
}

fn message(message: UnifiedMessage) -> SplitMessage {
    SplitMessage::Unified(message)
}

async fn send(panel: &mut SplitPanel, m: UnifiedMessage) {
    let task = panel.update(message(m));
    drive(panel, task).await;
}

/// A started panel at seed entry on the single-step route.
async fn at_seed_entry(
    scan: &Scan,
    connect: &Arc<FakeConnect>,
    temp: &Temp,
) -> (SplitPanel, Arc<FakePort>) {
    let mut panel = SplitPanel::start(TARGET.into(), temp.root(), scan.intent());
    panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
    let port = FakePort::new(connect.context(), 1);
    panel.set_unified_port(Some(port.clone() as Arc<dyn UnifiedPort>));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::ChooseRoute);
    assert!(panel.seed_route_offered());
    send(&mut panel, UnifiedMessage::Choose(Route::Seeds)).await;
    assert_eq!(
        panel.stage(),
        &Stage::Unified(UnifiedStage::EnterSeeds),
        "{:?}",
        panel.stage()
    );
    assert_eq!(*port.calls.opens.lock().unwrap(), vec![false]);
    (panel, port)
}

async fn add_seed(panel: &mut SplitPanel, byte: u8, passphrase: &str) {
    send(panel, UnifiedMessage::Words(words(byte))).await;
    send(
        panel,
        UnifiedMessage::Passphrase(SeedText::from(passphrase.to_owned())),
    )
    .await;
    send(panel, UnifiedMessage::AddSeed).await;
    // The buffers are emptied by every attempt.
    assert!(panel.unified().typed_words().is_empty());
    assert!(panel.unified().typed_passphrase().is_empty());
}

/// Two seeds of the 2-of-3 sign; the review shows only the Protected pill
/// and its limitation (the unified sweep is Protected by construction), the
/// route label, and no replayable-spend acknowledgement anywhere in the
/// Split panel's state or view. CF: drop the Protected pill from the
/// driver's review.
#[tokio::test(flavor = "multi_thread")]
async fn unified_flow_two_seeds_shows_protected_only() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let (mut panel, port) = at_seed_entry(&scan, &connect, &temp).await;
    assert_eq!(panel.unified().threshold(), 2);
    assert!(!panel.can_build_unified());
    add_seed(&mut panel, 1, "").await;
    assert_eq!(panel.unified().held(), 1, "{:?}", panel.notice());
    assert!(!panel.can_build_unified());
    add_seed(&mut panel, 2, "second passphrase").await;
    assert_eq!(panel.unified().held(), 2, "{:?}", panel.notice());
    assert!(panel.can_build_unified());

    send(&mut panel, UnifiedMessage::BuildAndSign).await;
    assert_eq!(
        panel.stage(),
        &Stage::Unified(UnifiedStage::Signed),
        "{:?}",
        panel.notice()
    );
    // Signed: the seeds are gone.
    assert!(!panel.unified().holds_seeds());
    assert_eq!(panel.unified().held(), 0);
    assert_eq!(port.calls.reserves.load(Ordering::SeqCst), 1);

    send(&mut panel, UnifiedMessage::Review).await;
    assert_eq!(panel.stage(), &Stage::Unified(UnifiedStage::Review));
    let review = panel.unified().review().unwrap().clone();
    assert_eq!(
        review.protected,
        replay::pill_copy(&replay::ReplayStatus::Protected, &[]).0
    );
    assert_eq!(review.limitation, replay::PROTECTED_LIMITATION);
    assert_eq!(review.route_label, SubmissionRoute::Connect.label());
    assert_eq!(review.privacy_note, None);
    // Nothing was recorded before confirmation (U3).
    assert!(step1::discover(&temp.root()).is_empty());

    send(&mut panel, UnifiedMessage::Confirm).await;
    assert_eq!(panel.stage(), &Stage::Unified(UnifiedStage::Submitted));
    assert_eq!(port.calls.confirms.load(Ordering::SeqCst), 1);
    assert!(matches!(
        panel.unified().outcome(),
        Some(Outcome::Uncertain { txid, .. }) if txid == review.txid
    ));
    send(&mut panel, UnifiedMessage::Reconcile).await;
    assert_eq!(panel.unified().seen(), Some(TransactionObservation::Absent));

    // No replayable-spend acknowledgement in the Split panel or its view.
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for path in [
        "app/state/vault/split/mod.rs",
        "app/state/vault/split/unified.rs",
        "app/state/vault/split/panel2.rs",
        "app/state/vault/split/step2.rs",
        "app/view/vault/split.rs",
    ] {
        let text = std::fs::read_to_string(src.join(path)).unwrap();
        assert!(!text.contains("REPLAYABLE_ACKNOWLEDGEMENT"), "{}", path);
        assert!(!text.contains("AcknowledgeReplay"), "{}", path);
    }
}

/// Cancel, Close and a revocation each clear the seed set, drop it and
/// empty both inputs. CF: no scrub on Close (`revoke` keeps the set).
#[tokio::test(flavor = "multi_thread")]
async fn unified_cancel_close_and_revoke_scrub_the_seed_set() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    for exit in ["cancel", "close", "revoke", "session"] {
        let (mut panel, port) = at_seed_entry(&scan, &connect, &temp).await;
        add_seed(&mut panel, 1, "").await;
        assert_eq!(panel.unified().held(), 1, "{}", exit);
        // Typed but not yet added.
        send(&mut panel, UnifiedMessage::Words(words(3))).await;
        send(
            &mut panel,
            UnifiedMessage::Passphrase(SeedText::from("typed".to_owned())),
        )
        .await;
        assert!(!panel.unified().typed_words().is_empty());
        match exit {
            "cancel" => {
                send(&mut panel, UnifiedMessage::Cancel).await;
                assert_eq!(panel.stage(), &Stage::ChooseRoute);
            }
            "close" => {
                let task = panel.update(SplitMessage::Close);
                drive(&mut panel, task).await;
                assert!(panel.is_hidden());
            }
            "revoke" => panel.revoke(),
            _ => panel.set_connect(None),
        }
        assert!(!panel.unified().holds_seeds(), "{}", exit);
        assert_eq!(panel.unified().held(), 0, "{}", exit);
        assert!(panel.unified().typed_words().is_empty(), "{}", exit);
        assert!(panel.unified().typed_passphrase().is_empty(), "{}", exit);
        assert!(port.calls.revoked.load(Ordering::SeqCst), "{}", exit);
        // Reviewer-660 F5: the revoked coordinator is dropped, not kept.
        assert!(panel.unified.flow.is_none(), "{}", exit);
        assert!(panel.unified.revoke.is_none(), "{}", exit);
        assert_eq!(port.calls.builds.load(Ordering::SeqCst), 0);
    }
}

/// A seed outside the wallet's policy (or a policy seed with the wrong
/// passphrase) is refused with copy and nothing is kept. The copy for every
/// seed refusal avoids "fingerprint", and the #647 O3 case says to clear the
/// seeds and enter them again. CF: remove the origin check in
/// `SeedSet::add`.
#[tokio::test(flavor = "multi_thread")]
async fn unified_refuses_seed_outside_policy_with_copy() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let (mut panel, _) = at_seed_entry(&scan, &connect, &temp).await;
    let unknown = describe_seed(&SeedSetError::UnknownOrigin);
    for (byte, passphrase) in [(9, ""), (2, "")] {
        add_seed(&mut panel, byte, passphrase).await;
        assert_eq!(panel.unified().held(), 0);
        assert_eq!(panel.notice(), Some(unknown.as_str()));
        assert_eq!(panel.stage(), &Stage::Unified(UnifiedStage::EnterSeeds));
    }
    let o3 = describe_seed(&SeedSetError::Signing(
        ForeignUnifiedError::DerivedPublicKeyMismatch {
            input: 0,
            public_key: coincube_core::miniscript::bitcoin::PublicKey::from_str(
                "02a1633cafcc01ebfb6d78e39f687a1f0995c62fc95f51ead10a02ee0be551b5dc",
            )
            .unwrap(),
        },
    ));
    assert!(
        o3.contains("Clear the seeds and enter them again"),
        "{}",
        o3
    );
    for error in [
        SeedSetError::UnsupportedPolicy,
        SeedSetError::Seed(crate::services::foreign_wallet_source::SourceError::Mnemonic),
        SeedSetError::UnknownOrigin,
        SeedSetError::Duplicate,
        SeedSetError::Full { threshold: 2 },
        SeedSetError::Incomplete { have: 1, need: 2 },
        SeedSetError::Signing(ForeignUnifiedError::NotBitcoinBlake2b(ChainId::Bitcoin)),
    ] {
        let copy = describe_seed(&error);
        assert!(!copy.is_empty());
        assert!(!copy.to_lowercase().contains("fingerprint"), "{}", copy);
    }
    assert!(!o3.to_lowercase().contains("fingerprint"));
}

/// P1: the single step is never offered for a hardware wallet; asking for
/// it shows the P1 copy and opens nothing. CF: remove the P1 branch.
#[tokio::test(flavor = "multi_thread")]
async fn hardware_single_step_route_is_blocked() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let mut panel = SplitPanel::start(TARGET.into(), temp.root(), scan.intent());
    panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
    let port = FakePort::new(connect.context(), 1);
    panel.set_unified_port(Some(port.clone() as Arc<dyn UnifiedPort>));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::ChooseRoute);
    send(&mut panel, UnifiedMessage::Choose(Route::Hardware)).await;
    assert_eq!(panel.stage(), &Stage::ChooseRoute);
    assert_eq!(panel.notice(), Some(P1_HARDWARE));
    assert!(port.calls.opens.lock().unwrap().is_empty());
    assert!(!panel.unified().holds_seeds());
    // The view offers no hardware single step.
    let labels = crate::app::state::vault::split::device::tests::rendered_labels(&panel).await;
    assert!(labels.iter().any(|l| l == "One step with recovery phrases"));
    assert!(labels.iter().any(|l| l == P1_HARDWARE));
    assert!(
        !labels
            .iter()
            .any(|l| l.to_lowercase().contains("hardware") && l.len() < 40),
        "{:?}",
        labels
    );
}

/// D1: no message reaches the route choice outside a started panel. A
/// resumed panel ignores it, and the single-step messages have no "start".
#[tokio::test(flavor = "multi_thread")]
async fn unified_route_is_reached_only_from_a_started_panel() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let digest = sha256::Hash::from_byte_array([3; 32]);
    let mut panel = SplitPanel::resume(
        TARGET.into(),
        temp.root(),
        digest,
        step1::journal_directory(&temp.root(), digest),
    );
    panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
    let port = FakePort::new(connect.context(), 1);
    panel.set_unified_port(Some(port.clone() as Arc<dyn UnifiedPort>));
    let before = panel.stage().clone();
    for route in [Route::Seeds, Route::TwoStep, Route::Hardware] {
        send(&mut panel, UnifiedMessage::Choose(route)).await;
        assert_eq!(panel.stage(), &before);
    }
    send(&mut panel, UnifiedMessage::AddSeed).await;
    send(&mut panel, UnifiedMessage::BuildAndSign).await;
    assert!(port.calls.opens.lock().unwrap().is_empty());
    let state = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/app/state/vault/split/unified.rs"),
    )
    .unwrap();
    let intents = &state[state.find("pub enum UnifiedMessage {").unwrap()..];
    let intents = &intents[..intents.find("\n}\n").unwrap()];
    assert!(!intents.to_lowercase().contains("start"));
}

/// #654 F2 (lead decision): the review reads the D4 fee again. A signed
/// rate below it, or no estimate, is refused before anything is journaled
/// and the refused review can't be confirmed; below the estimate the sweep
/// is built and signed again. CF: drop the fee floor at the review.
#[tokio::test(flavor = "multi_thread")]
async fn unified_review_rereads_the_d4_fee_before_any_journal_write() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    for (estimate, copy, stage) in [
        (Some(500), FEE_BELOW_ESTIMATE, UnifiedStage::EnterSeeds),
        (None, FEE_UNAVAILABLE_AT_REVIEW, UnifiedStage::Signed),
    ] {
        let (mut panel, port) = at_seed_entry(&scan, &connect, &temp).await;
        add_seed(&mut panel, 1, "").await;
        add_seed(&mut panel, 3, "").await;
        send(&mut panel, UnifiedMessage::BuildAndSign).await;
        assert_eq!(panel.stage(), &Stage::Unified(UnifiedStage::Signed));
        *port.calls.review_feerate.lock().unwrap() = estimate;
        send(&mut panel, UnifiedMessage::Review).await;
        assert_eq!(panel.notice(), Some(copy));
        assert_eq!(panel.stage(), &Stage::Unified(stage));
        assert!(panel.unified().review().is_none());
        send(&mut panel, UnifiedMessage::Confirm).await;
        assert_eq!(port.calls.confirms.load(Ordering::SeqCst), 0);
        assert!(step1::discover(&temp.root()).is_empty());
    }
}

/// A real fork-only journal of `scan`'s coins under `temp`: created, and
/// with its submission recorded when `submitted` (signed by seeds 1 and 3).
fn fork_only_journal(
    scan: &Scan,
    connect: &FakeConnect,
    temp: &Temp,
    submitted: bool,
) -> (sha256::Hash, PathBuf, Option<Txid>) {
    let source = split_source(&scan.wallet.external, Some(&scan.wallet.internal)).unwrap();
    let digest = source.digest();
    let directory = step1::journal_directory(&temp.root(), digest);
    let inventory = scan.intent().inventory;
    let open = UnifiedOpen {
        directory: directory.clone(),
        target_cube: TARGET.into(),
        source,
        coins: inventory.splittable_coins(),
        fork_height: fixture::FORK,
    };
    let sweep = build_sweep(&open, 2);
    claim_workflow::prepare_directory(&directory).unwrap();
    let mut controller =
        Controller::create_unified_split(&directory, TARGET.into(), &sweep, 7, connect.context())
            .unwrap();
    if !submitted {
        return (digest, directory, None);
    }
    let mut seeds = SeedSet::new(&open.source).unwrap();
    seeds
        .add(
            Zeroizing::new(mnemonic(1).to_string()),
            Zeroizing::default(),
        )
        .unwrap();
    seeds
        .add(
            Zeroizing::new(mnemonic(3).to_string()),
            Zeroizing::default(),
        )
        .unwrap();
    let unsigned = UnifiedPsbt::from_psbt(sweep.psbt().clone()).unwrap();
    let signed = seeds
        .sign_unified(&unsigned, ChainId::BitcoinBlake2b, &Secp256k1::new())
        .unwrap();
    seeds.clear();
    let verified =
        finalize_unified_sweep(&sweep, &signed, &Secp256k1::verification_only()).unwrap();
    controller
        .record_unified_broadcast_intent(&connect.context(), &verified)
        .unwrap();
    (
        digest,
        directory,
        Some(verified.transaction().compute_txid()),
    )
}

fn resumed(digest: sha256::Hash, directory: PathBuf, temp: &Temp) -> SplitPanel {
    SplitPanel::resume(TARGET.into(), temp.root(), digest, directory)
}

/// Restart by kind: a fork-only record never reaches step 1's restore or
/// the two-step reconciler. With a recorded submission it opens the unified
/// reconciler (Reconcile); without one it is revalidated (coins
/// authenticated afresh, the coordinator resumed) and goes back to seed
/// entry. CF: decide only on whether a step 2 is recorded.
#[tokio::test(flavor = "multi_thread")]
async fn restart_routes_by_kind() {
    let scan = seed_scan();
    for submitted in [true, false] {
        let connect = FakeConnect::new(&scan.coins);
        let temp = Temp::new();
        let (digest, directory, sweep) = fork_only_journal(&scan, &connect, &temp, submitted);
        let restarted = step2::restart(
            connect.context(),
            None,
            None,
            directory.clone(),
            TARGET.into(),
            digest,
        )
        .await;
        match restarted {
            Ok(step2::Restart::Unified(record)) => {
                assert_eq!(record.sweep, sweep);
                assert_eq!(record.claimed.len(), scan.coins.len());
            }
            Ok(_) => panic!("submitted={}: not opened by kind", submitted),
            Err(refusal) => panic!("submitted={}: {:?}", submitted, refusal),
        }

        let mut panel = resumed(digest, directory.clone(), &temp);
        panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
        let port = FakePort::new(connect.context(), 1);
        panel.set_unified_port(Some(port.clone() as Arc<dyn UnifiedPort>));
        let task = panel.begin();
        drive(&mut panel, task).await;
        if submitted {
            assert_eq!(panel.stage(), &Stage::Unified(UnifiedStage::Reconcile));
            assert_eq!(port.calls.recon_opens.load(Ordering::SeqCst), 1);
            assert!(port.calls.opens.lock().unwrap().is_empty());
        } else {
            assert_eq!(
                panel.stage(),
                &Stage::Unified(UnifiedStage::EnterSeeds),
                "{:?}",
                panel.stage()
            );
            assert_eq!(*port.calls.opens.lock().unwrap(), vec![true]);
            assert_eq!(panel.unified().threshold(), 2);
        }
        // Step 1's coordinator was never opened for it.
        assert!(connect.calls.opened.lock().unwrap().is_empty());
        // Without the unified port, nothing else opens it either.
        let mut panel = resumed(digest, directory, &temp);
        panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
        let task = panel.begin();
        drive(&mut panel, task).await;
        assert!(
            matches!(panel.stage(), Stage::Refused(refusal) if refusal.retry),
            "{:?}",
            panel.stage()
        );
        assert!(connect.calls.opened.lock().unwrap().is_empty());
    }
}

/// C6, U4: the fork-only close. A submitted record closes only after a
/// reconcile under this session saw the sweep absent and a fresh check
/// found it absent with every coin unspent on BTCB2; a sighting or a spent
/// coin refuses and keeps it. An unsubmitted record closes with no check.
/// The close writes the tombstone and keeps the journal. CF: drop the
/// coin check from `check_close`.
#[tokio::test(flavor = "multi_thread")]
async fn fork_only_close_needs_absence_and_unspent_coins() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let (digest, directory, sweep) = fork_only_journal(&scan, &connect, &temp, true);
    let sweep = sweep.unwrap();
    let record = UnifiedRecord {
        sweep: Some(sweep),
        claimed: scan.coins.iter().map(|coin| coin.outpoint).collect(),
    };
    check_close(&*connect, &record).await.unwrap();
    // A sighting refuses.
    connect.chains.status.lock().unwrap().insert(
        (ChainId::BitcoinBlake2b, sweep),
        TransactionObservation::Unconfirmed { txid: sweep },
    );
    assert_eq!(
        check_close(&*connect, &record).await.unwrap_err().reason,
        SWEEP_SEEN
    );
    connect
        .chains
        .status
        .lock()
        .unwrap()
        .remove(&(ChainId::BitcoinBlake2b, sweep));
    // A coin spent on BTCB2 refuses.
    let spent = FakeConnect::new(&scan.coins);
    spent
        .chains
        .spend_on(ChainId::BitcoinBlake2b, scan.coins[1].outpoint);
    assert_eq!(
        check_close(&*spent, &record).await.unwrap_err().reason,
        COIN_SPENT_ON_BTCB2
    );

    // The panel: no close before a reconcile saw the sweep absent.
    let mut panel = resumed(digest, directory.clone(), &temp);
    panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
    let port = FakePort::new(connect.context(), 1);
    panel.set_unified_port(Some(port.clone() as Arc<dyn UnifiedPort>));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Unified(UnifiedStage::Reconcile));
    assert!(!panel.can_check_unified_close());
    assert!(!panel.can_confirm_unified_close());
    *port.calls.seen.lock().unwrap() = Some(TransactionObservation::Unconfirmed { txid: sweep });
    send(&mut panel, UnifiedMessage::Reconcile).await;
    assert!(!panel.can_check_unified_close());
    *port.calls.seen.lock().unwrap() = Some(TransactionObservation::Absent);
    send(&mut panel, UnifiedMessage::Reconcile).await;
    assert!(panel.can_check_unified_close());
    assert!(!panel.can_confirm_unified_close());
    send(&mut panel, UnifiedMessage::CheckClose).await;
    assert!(panel.can_confirm_unified_close(), "{:?}", panel.notice());
    send(&mut panel, UnifiedMessage::ConfirmClose).await;
    assert_eq!(panel.stage(), &Stage::Closed, "{:?}", panel.stage());
    assert!(port.calls.revoked.load(Ordering::SeqCst));
    assert!(step1::is_closed(&directory));
    assert!(directory.join("intent.json").exists());
    assert!(step1::discover(&temp.root()).is_empty());

    // A record that no longer matches what was checked is not closed.
    let temp = Temp::new();
    let (digest, directory, _) = fork_only_journal(&scan, &connect, &temp, true);
    let ended = AtomicBool::new(false);
    let other = UnifiedRecord {
        sweep: Some(Txid::all_zeros()),
        claimed: record.claimed.clone(),
    };
    assert_eq!(
        step2::close_unified(
            &directory,
            TARGET,
            digest,
            connect.context(),
            &other,
            1,
            &ended
        ),
        Err(step2::CHANGED_SINCE_CHECK.to_string())
    );
    assert!(!step1::is_closed(&directory));

    // Unsubmitted: closed from seed entry with no chain check.
    let temp = Temp::new();
    let (digest, directory, _) = fork_only_journal(&scan, &connect, &temp, false);
    let mut panel = resumed(digest, directory.clone(), &temp);
    panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
    let port = FakePort::new(connect.context(), 1);
    panel.set_unified_port(Some(port.clone() as Arc<dyn UnifiedPort>));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Unified(UnifiedStage::EnterSeeds));
    assert!(!panel.can_check_unified_close());
    assert!(panel.can_confirm_unified_close());
    add_seed(&mut panel, 1, "").await;
    send(&mut panel, UnifiedMessage::ConfirmClose).await;
    assert_eq!(panel.stage(), &Stage::Closed);
    assert!(!panel.unified().holds_seeds());
    assert!(step1::is_closed(&directory));
}

/// The abandon path names the single-step route for a fork-only record,
/// whose abandon the journal refuses with `Conflict`.
#[tokio::test(flavor = "multi_thread")]
async fn fork_only_abandon_conflict_gets_fork_only_copy() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let (digest, directory, _) = fork_only_journal(&scan, &connect, &temp, false);
    let ended = AtomicBool::new(false);
    let error = step1::abandon(&directory, TARGET, digest, connect.context(), &ended).unwrap_err();
    assert!(
        matches!(error, claim_workflow::Error::Conflict),
        "{:?}",
        error
    );
    assert_eq!(
        abandon_refusal(&directory, TARGET, digest, connect.context(), error),
        FORK_ONLY_ABANDON
    );
    assert!(directory.join("intent.json").exists());
}

/// The unified port is the session's: an equivalent one keeps the route;
/// another (another daemon, or none) revokes the coordinator and scrubs the
/// seeds, held or in a task. CF: no revocation on a changed port.
#[tokio::test(flavor = "multi_thread")]
async fn unified_port_is_built_per_session_and_revoked() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let (mut panel, port) = at_seed_entry(&scan, &connect, &temp).await;
    add_seed(&mut panel, 1, "").await;
    // The same session and daemon: nothing changes.
    let same = FakePort::new(connect.context(), 1);
    panel.set_unified_port(Some(same as Arc<dyn UnifiedPort>));
    assert!(!port.calls.revoked.load(Ordering::SeqCst));
    assert_eq!(panel.unified().held(), 1);
    // Another daemon instance: revoked and scrubbed.
    let other = FakePort::new(connect.context(), 2);
    panel.set_unified_port(Some(other as Arc<dyn UnifiedPort>));
    assert!(port.calls.revoked.load(Ordering::SeqCst));
    assert!(!panel.unified().holds_seeds());
    assert_eq!(panel.stage(), &Stage::NeedsSession);
    // A seed being added in a task when the port goes: its result is dropped.
    let (mut panel, port) = at_seed_entry(&scan, &connect, &temp).await;
    send(&mut panel, UnifiedMessage::Words(words(1))).await;
    let task = panel.update(message(UnifiedMessage::AddSeed));
    panel.set_unified_port(None);
    assert!(port.calls.revoked.load(Ordering::SeqCst));
    drive(&mut panel, task).await;
    assert!(!panel.unified().holds_seeds());
    assert_eq!(panel.unified().held(), 0);
}

/// The seeds are held only in zeroizing memory: `split/unified.rs` and the
/// Split view touch no file, configuration, encrypted store or session
/// cache; the seed newtype's `Debug` is written by hand and redacts; the
/// view's only text inputs are the seed inputs, each `.secure(true)`, and
/// they carry their text straight into [`SeedText`].
#[test]
fn split_unified_holds_seeds_only_zeroized() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let read = |path: &str| std::fs::read_to_string(src.join(path)).unwrap();
    let unified = read("app/state/vault/split/unified.rs");
    let unified = &unified[..unified
        .find("\n#[cfg(all(test, unix))]\nmod tests;")
        .unwrap()];
    let view = read("app/view/vault/split.rs");
    let seed_text = read("app/state/vault/split/unified/seed_text.rs");
    for (file, text) in [
        ("unified.rs", unified),
        ("seed_text.rs", seed_text.as_str()),
        ("view", view.as_str()),
    ] {
        for token in [
            "fs::",
            "serde",
            "store_encrypted",
            "store_unlocked_signer",
            "session::",
            "seed_source::",
        ] {
            assert!(!text.contains(token), "{} names {}", file, token);
        }
    }
    // Reviewer-660661e D2 (structural): `SeedText` lives in a private module
    // of `unified`, with a private field, so nothing outside its impl can
    // read the buffer (`.0`): the compiler refuses it. Its module exports
    // only the type, through `unified`.
    assert!(unified.contains("\nmod seed_text;\npub use seed_text::SeedText;\n"));
    assert!(!unified.contains("pub mod seed_text"));
    assert!(
        !unified.contains("pub(crate) mod seed_text")
            && !unified.contains("pub(super) mod seed_text")
    );
    assert!(!unified.contains("struct SeedText"));
    assert!(seed_text.contains("pub struct SeedText(Zeroizing<String>);"));
    // The field is not public, and `SeedText` is the module's only type.
    assert_eq!(seed_text.matches("struct ").count(), 1);
    assert_eq!(seed_text.matches("pub struct SeedText(pub").count(), 0);
    // No derived Debug on the seed newtype; its own redacts.
    let at = seed_text
        .find("pub struct SeedText(Zeroizing<String>);")
        .unwrap();
    let attributes = &seed_text[seed_text[..at].rfind("\n\n").unwrap()..at];
    assert!(!attributes.contains("Debug"), "{}", attributes);
    assert!(seed_text.contains("impl fmt::Debug for SeedText"));
    let secret = SeedText::from(mnemonic(1).to_string());
    let printed = format!(
        "{:?} {:?}",
        secret,
        SplitMessage::Unified(UnifiedMessage::Words(secret.clone()))
    );
    assert!(!printed.contains(&*mnemonic(1).to_string()));
    assert!(printed.contains("<redacted>"));
    // Reviewer-660 F2 and Reviewer-660661d D1: no copy of the typed text,
    // anywhere in the app. The text is read only through
    // `SeedText::expose_for_secure_input`, which the view's one secure input
    // calls; the buffers are lent only through `typed_words` and
    // `typed_passphrase`, which only the view's seed stage calls. Every
    // non-test source file under `src` is scanned.
    fn sources(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<(String, String)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            if path.is_dir() {
                if name != "tests" {
                    sources(&path, root, out);
                }
            } else if name.ends_with(".rs") && name != "tests.rs" {
                let text = std::fs::read_to_string(&path)
                    .unwrap()
                    .replace("\r\n", "\n");
                // Production text: up to an inline test module.
                let cut = ["\n#[cfg(test)]\nmod ", "\n#[cfg(all(test"]
                    .iter()
                    .filter_map(|marker| text.find(marker))
                    .min()
                    .unwrap_or(text.len());
                let file = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((file, text[..cut].to_string()));
            }
        }
    }
    let mut all = Vec::new();
    sources(&src, &src, &mut all);
    assert!(all.len() > 100, "{}", all.len());
    for (token, sites) in [
        (
            "expose_for_secure_input(",
            [
                ("app/state/vault/split/unified/seed_text.rs", 1),
                ("app/view/vault/split.rs", 1),
            ],
        ),
        (
            "typed_words(",
            [
                ("app/state/vault/split/unified.rs", 1),
                ("app/view/vault/split.rs", 1),
            ],
        ),
        (
            "typed_passphrase(",
            [
                ("app/state/vault/split/unified.rs", 1),
                ("app/view/vault/split.rs", 1),
            ],
        ),
    ] {
        let found: Vec<(String, usize)> = all
            .iter()
            .map(|(file, text)| (file.clone(), text.matches(token).count()))
            .filter(|(_, n)| *n > 0)
            .collect();
        let expected: Vec<(String, usize)> =
            sites.iter().map(|(f, n)| (f.to_string(), *n)).collect();
        let mut found = found;
        found.sort();
        assert_eq!(found, expected, "{}", token);
    }
    // No other read of the text: `SeedText` has no `as_str`, `Deref` or
    // `AsRef`, and its module reads `.0` only in its own methods.
    assert_eq!(unified.matches("as_str()").count(), 0);
    assert_eq!(seed_text.matches("as_str").count(), 0);
    for token in [
        "impl Deref",
        "impl std::ops::Deref",
        "impl AsRef",
        "impl Borrow",
        "impl fmt::Display",
    ] {
        assert!(!seed_text.contains(token), "seed_text.rs has {}", token);
    }
    // In `seed_text.rs`: the definition only.
    assert!(seed_text.contains("pub(in crate::app) fn expose_for_secure_input(&self) -> &str {"));
    // In the view: inside `seed_input`, as the secure input's value; and the
    // two buffers handed to it in the seed stage.
    let input = &view[view.find("fn seed_input<").unwrap()..];
    let input = &input[..input.find("\n}\n").unwrap()];
    assert!(input.contains(
        "text_input(placeholder, value.expose_for_secure_input())\n        .secure(true)"
    ));
    let stage = &view[view.find("fn unified_body<").unwrap()..];
    let stage = &stage[..stage.find("\n}\n").unwrap()];
    assert!(stage
        .contains("seed_input(\"Recovery phrase\", state.typed_words(), UnifiedMessage::Words)"));
    assert!(stage
        .contains("state.typed_passphrase(),\n                    UnifiedMessage::Passphrase,"));
    // The moved-out buffers flow only into the seed set: in the AddSeed arm,
    // `words` and `passphrase` are bound from `take()` and used only as
    // `seeds.add(words, passphrase)`.
    let arm = &unified[unified.find("            UnifiedMessage::AddSeed").unwrap()..];
    let arm = &arm[..arm.find("            UnifiedMessage::ClearSeeds").unwrap()];
    let mut rest = arm.to_string();
    for allowed in [
        "self.unified.words.is_empty()",
        "let (words, passphrase) =",
        "(self.unified.words.take(), self.unified.passphrase.take())",
        "seeds.add(words, passphrase)",
    ] {
        assert_eq!(rest.matches(allowed).count(), 1, "{}", allowed);
        rest = rest.replace(allowed, "");
    }
    for ident in ["words", "passphrase"] {
        let mut at = 0;
        while let Some(found) = rest[at..].find(ident) {
            let start = at + found;
            let end = start + ident.len();
            let before = rest[..start].chars().last().unwrap_or(' ');
            let after = rest[end..].chars().next().unwrap_or(' ');
            let word = !(before.is_alphanumeric() || before == '_')
                && !(after.is_alphanumeric() || after == '_');
            assert!(
                !word,
                "the moved-out `{}` is used beyond `seeds.add`: {:?}",
                ident,
                &rest[start.saturating_sub(40)..(end + 40).min(rest.len())]
            );
            at = end;
        }
    }
    // Nothing in the route logs or prints.
    for token in [
        "tracing::",
        "log::",
        "println!",
        "eprintln!",
        "print!",
        "dbg!",
        "info!(",
        "debug!(",
        "warn!(",
        "error!(",
        "trace!(",
    ] {
        assert!(!unified.contains(token), "unified.rs names {}", token);
    }
    for field in ["words", "passphrase"] {
        let needle = format!("unified.{}", field);
        for (at, _) in unified.match_indices(&needle) {
            let rest = &unified[at + needle.len()..];
            assert!(
                [".take()", ".clear()", " = text;", ".is_empty()"]
                    .iter()
                    .any(|use_| rest.starts_with(use_)),
                "{} used as {:?}",
                needle,
                &rest[..rest.len().min(40)]
            );
        }
        // Inside `UnifiedState`: emptied by `scrub`, or lent by the accessor.
        let own = format!("self.{}", field);
        for (at, _) in unified.match_indices(&own) {
            let rest = &unified[at + own.len()..];
            assert!(
                rest.starts_with(".clear();") || rest.starts_with("\n    }"),
                "{} used as {:?}",
                own,
                &rest[..rest.len().min(40)]
            );
        }
    }
    // Every text input in the view is the seed input, `.secure(true)`, and
    // both seed fields use it.
    let code: String = view
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(code.matches(".secure(").count(), 1);
    assert_eq!(code.matches(".secure(true)").count(), 1);
    assert_eq!(view.matches("text_input(").count(), 1);
    let input = &view[view.find("fn seed_input<").unwrap()..];
    let input = &input[..input.find("\n}\n").unwrap()];
    assert!(input.contains("SeedText::from(typed)"));
    let body = &view[view.find("fn unified_body<").unwrap()..];
    let body = &body[..body.find("\n}\n").unwrap()];
    assert_eq!(body.matches("seed_input(").count(), 2);
    // The messages carry the secret type, never a String.
    let intents = &unified[unified.find("pub enum UnifiedMessage {").unwrap()..];
    let intents = &intents[..intents.find("\n}\n").unwrap()];
    assert!(intents.contains("Words(SeedText)") && intents.contains("Passphrase(SeedText)"));
    assert!(!intents.contains("String"));
}

/// An origin-less 1-key wallet: its descriptors give no seed route (U6).
fn bare_scan() -> Scan {
    use coincube_core::miniscript::bitcoin::bip32::Xpub;
    let secp = Secp256k1::new();
    let master = fixture::master(1);
    let account = master
        .derive_priv(&secp, &DerivationPath::from_str("m/84'/0'/0'").unwrap())
        .unwrap();
    let xpub = Xpub::from_priv(&secp, &account);
    let parse = |branch, step: u32| {
        ScanDescriptor::parse(branch, &format!("wpkh({}/{}/*)", xpub, step)).unwrap()
    };
    let wallet = fixture::Wallet {
        external: parse(Branch::External, 0),
        internal: parse(Branch::Internal, 1),
        signers: Vec::new(),
    };
    let coins = fixture::shared_coins(&wallet);
    Scan { wallet, coins }
}

/// Reviewer-660 F1: every single-step precondition refuses with its copy
/// and opens nothing. U6: a wallet without the seed route is not offered
/// it, and both the route choice and `preconditions` refuse it. P7 (U7): a
/// fixed wallet and an unproven fresh index are refused. A changed fork
/// height is a stale anchor. CF: R12a (the offer ignores the routes), R12b
/// (`preconditions` skips U6), R13 (a fixed wallet admitted), R14 (no
/// fork-height check).
#[tokio::test(flavor = "multi_thread")]
async fn unified_preconditions_refuse_with_copy_and_open_nothing() {
    use crate::services::foreign_split_inventory::SplitInventory;
    // U6, at the offer and at both checks.
    let bare = bare_scan();
    let connect = FakeConnect::new(&bare.coins);
    let temp = Temp::new();
    let intent = bare.intent();
    assert!(!intent_routes(&intent).seed_unified);
    let refused = preconditions(&*connect, &intent, &temp.root(), TARGET.into())
        .await
        .err()
        .expect("no seed route");
    assert_eq!(refused.reason, SEEDS_NOT_OFFERED);
    let mut panel = SplitPanel::start(TARGET.into(), temp.root(), intent);
    panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
    let port = FakePort::new(connect.context(), 1);
    panel.set_unified_port(Some(port.clone() as Arc<dyn UnifiedPort>));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::ChooseRoute);
    assert!(!panel.seed_route_offered());
    send(&mut panel, UnifiedMessage::Choose(Route::Seeds)).await;
    assert_eq!(panel.stage(), &Stage::ChooseRoute);
    assert_eq!(panel.notice(), Some(SEEDS_NOT_OFFERED));
    assert!(port.calls.opens.lock().unwrap().is_empty());

    // P7 and the fork height, on a wallet that has the seed route.
    let scan = seed_scan();
    let fixed = {
        let mut intent = scan.intent();
        let btcb2 = fixture::report(ChainId::BitcoinBlake2b, scan.coins.clone());
        let bitcoin = fixture::report(ChainId::Bitcoin, scan.coins.clone());
        intent.inventory =
            SplitInventory::join(&btcb2, &bitcoin, fixture::GENERATION, false).unwrap();
        intent
    };
    let unproven = scan.intent_with(|report| report.with_coverage(fixture::walk(4, Some(3))));
    type Case = (&'static str, SplitIntent, bool, &'static str);
    let cases: Vec<Case> = vec![
        ("fixed wallet (P7)", fixed, false, step1::FIXED_WALLET),
        (
            "unproven fresh index (P7)",
            unproven,
            false,
            step1::WATCH_ONLY_DEFERRED,
        ),
        (
            "changed fork height",
            scan.intent(),
            true,
            step1::STALE_ANCHOR,
        ),
    ];
    for (name, intent, stale, copy) in cases {
        assert!(intent_routes(&intent).seed_unified, "{}", name);
        let connect = FakeConnect::new(&scan.coins);
        if stale {
            let mut window = connect.window.lock().unwrap().clone().unwrap();
            window.fork_height += 1;
            *connect.window.lock().unwrap() = Ok(window);
        }
        let temp = Temp::new();
        let mut panel = SplitPanel::start(TARGET.into(), temp.root(), intent);
        panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
        let port = FakePort::new(connect.context(), 1);
        panel.set_unified_port(Some(port.clone() as Arc<dyn UnifiedPort>));
        let task = panel.begin();
        drive(&mut panel, task).await;
        assert!(panel.seed_route_offered(), "{}", name);
        send(&mut panel, UnifiedMessage::Choose(Route::Seeds)).await;
        match panel.stage() {
            Stage::Refused(refusal) => assert_eq!(refusal.reason, copy, "{}", name),
            other => panic!("{}: {:?}", name, other),
        }
        assert!(port.calls.opens.lock().unwrap().is_empty(), "{}", name);
        assert!(!panel.unified().holds_seeds(), "{}", name);
    }
}

/// Reviewer-660 F3: "Clear phrases" clears the set and empties both inputs,
/// stays at seed entry, and the same phrase can then be added again (so the
/// set really is empty). CF: R15 (the arm clears nothing).
#[tokio::test(flavor = "multi_thread")]
async fn unified_clear_phrases_scrubs_the_set_and_inputs() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let (mut panel, _) = at_seed_entry(&scan, &connect, &temp).await;
    add_seed(&mut panel, 1, "").await;
    assert_eq!(panel.unified().held(), 1);
    send(&mut panel, UnifiedMessage::Words(words(3))).await;
    send(
        &mut panel,
        UnifiedMessage::Passphrase(SeedText::from("typed".to_owned())),
    )
    .await;
    send(&mut panel, UnifiedMessage::ClearSeeds).await;
    assert_eq!(panel.stage(), &Stage::Unified(UnifiedStage::EnterSeeds));
    assert_eq!(panel.unified().held(), 0);
    assert_eq!(panel.unified.seeds.as_ref().map(SeedSet::len), Some(0));
    assert!(panel.unified().typed_words().is_empty());
    assert!(panel.unified().typed_passphrase().is_empty());
    add_seed(&mut panel, 1, "").await;
    assert_eq!(panel.unified().held(), 1, "{:?}", panel.notice());
    assert_eq!(panel.notice(), None);
}

/// Reviewer-660 F4: on the node route the review shows the node's label and
/// the single step's privacy note. CF: R16 (the note dropped).
#[tokio::test(flavor = "multi_thread")]
async fn unified_review_on_the_node_route_shows_the_privacy_note() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let (mut panel, port) = at_seed_entry(&scan, &connect, &temp).await;
    let node = SubmissionRoute::BitcoinNode {
        address: "127.0.0.1:8332".parse().unwrap(),
        identity: crate::services::claim_coordinator::NodeIdentity::for_test(1),
    };
    *port.calls.route.lock().unwrap() = Some(node);
    add_seed(&mut panel, 1, "").await;
    add_seed(&mut panel, 3, "").await;
    send(&mut panel, UnifiedMessage::BuildAndSign).await;
    send(&mut panel, UnifiedMessage::Review).await;
    assert_eq!(panel.stage(), &Stage::Unified(UnifiedStage::Review));
    let review = panel.unified().review().unwrap();
    assert_eq!(review.route_label, node.label());
    assert_eq!(review.privacy_note, Some(UNIFIED_NODE_PRIVACY));
    assert_eq!(
        review.protected,
        replay::pill_copy(&replay::ReplayStatus::Protected, &[]).0
    );
}

/// Reviewer-661 F1 (adopted probe): a failed or stale BTCB2 read in the
/// close check refuses with retry, for the sweep read and for the coin
/// read. CF: R3 (a stale coin read accepted), R10 (a failed sweep read
/// counted as absent).
#[tokio::test(flavor = "multi_thread")]
async fn fork_only_close_check_refuses_stale_and_failed_btcb2_reads() {
    use crate::app::state::vault::split::tests::Fault;
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let (_, _, sweep) = fork_only_journal(&scan, &connect, &temp, true);
    let record = UnifiedRecord {
        sweep,
        claimed: scan.coins.iter().map(|coin| coin.outpoint).collect(),
    };
    check_close(&*connect, &record).await.unwrap();
    for fault in [Fault::Stale, Fault::Error] {
        *connect.chains.b2_tx_fault.lock().unwrap() = Some(fault);
        let refusal = check_close(&*connect, &record).await.unwrap_err();
        assert!(refusal.retry, "tx {:?}: {}", fault, refusal.reason);
        *connect.chains.b2_tx_fault.lock().unwrap() = None;
        *connect.chains.b2_utxo_fault.lock().unwrap() = Some(fault);
        let refusal = check_close(&*connect, &record).await.unwrap_err();
        assert!(refusal.retry, "utxo {:?}: {}", fault, refusal.reason);
        *connect.chains.b2_utxo_fault.lock().unwrap() = None;
    }
}

/// Reviewer-661 F1 (adopted probe): a sweep that appears only after the
/// coin reads is caught by the second absence read. CF: R4.
#[tokio::test(flavor = "multi_thread")]
async fn fork_only_close_check_reads_the_sweep_again() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let (_, _, sweep) = fork_only_journal(&scan, &connect, &temp, true);
    let record = UnifiedRecord {
        sweep,
        claimed: scan.coins.iter().map(|coin| coin.outpoint).collect(),
    };
    connect.chains.b2_reads.store(0, Ordering::SeqCst);
    *connect.chains.b2_appear_after.lock().unwrap() = Some(1);
    assert_eq!(
        check_close(&*connect, &record).await.unwrap_err().reason,
        SWEEP_SEEN
    );
}

/// Reviewer-661 F1 (adopted probe): a coin spent on BTCB2 after the check
/// passed, before the confirmation, keeps the record open (the
/// confirmation checks again). CF: R8.
#[tokio::test(flavor = "multi_thread")]
async fn fork_only_confirm_close_checks_again() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let (digest, directory, _) = fork_only_journal(&scan, &connect, &temp, true);
    let mut panel = resumed(digest, directory.clone(), &temp);
    panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
    let port = FakePort::new(connect.context(), 1);
    panel.set_unified_port(Some(port.clone() as Arc<dyn UnifiedPort>));
    let task = panel.begin();
    drive(&mut panel, task).await;
    *port.calls.seen.lock().unwrap() = Some(TransactionObservation::Absent);
    send(&mut panel, UnifiedMessage::Reconcile).await;
    send(&mut panel, UnifiedMessage::CheckClose).await;
    assert!(panel.can_confirm_unified_close());
    connect
        .chains
        .spend_on(ChainId::BitcoinBlake2b, scan.coins[0].outpoint);
    send(&mut panel, UnifiedMessage::ConfirmClose).await;
    assert_ne!(panel.stage(), &Stage::Closed);
    assert!(!step1::is_closed(&directory));
}

/// Reviewer-661 F2: under the journal lock, `close_unified` refuses a
/// two-step journal even when handed that journal's own record (the kind
/// check, R7), and refuses once the session ended (`ended`, R9); neither
/// writes a tombstone.
#[tokio::test(flavor = "multi_thread")]
async fn close_unified_refuses_a_two_step_journal_and_an_ended_session() {
    // R7: a two-step journal with its own record.
    let scan = crate::app::state::vault::split::tests::Scan::new(
        crate::services::split_test_wallets::Shape::Wpkh,
    );
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let panel = crate::app::state::vault::split::tests::recorded(&scan, &connect, &temp).await;
    let (digest, directory) = (
        panel.journal.as_ref().unwrap().0,
        panel.journal.as_ref().unwrap().1.clone(),
    );
    drop(panel);
    let identity = claim_workflow::split_identity(TARGET.into(), digest);
    let record = {
        let controller =
            Controller::reopen_settling_blocking(&directory, &identity, connect.context()).unwrap();
        UnifiedRecord::of(&controller)
    };
    let ended = AtomicBool::new(false);
    assert_eq!(
        step2::close_unified(
            &directory,
            TARGET,
            digest,
            connect.context(),
            &record,
            1,
            &ended
        ),
        Err(step2::CHANGED_SINCE_CHECK.to_string())
    );
    assert!(!step1::is_closed(&directory));

    // R9: a fork-only journal, its own record, but the session ended.
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let (digest, directory, sweep) = fork_only_journal(&scan, &connect, &temp, true);
    let identity = claim_workflow::split_identity(TARGET.into(), digest);
    let record = {
        let controller =
            Controller::reopen_settling_blocking(&directory, &identity, connect.context()).unwrap();
        UnifiedRecord::of(&controller)
    };
    assert_eq!(record.sweep, sweep);
    let ended = AtomicBool::new(true);
    assert_eq!(
        step2::close_unified(
            &directory,
            TARGET,
            digest,
            connect.context(),
            &record,
            1,
            &ended
        ),
        Err(step1::ENDED_BEFORE_ABANDON.to_string())
    );
    assert!(!step1::is_closed(&directory));
    // The same call under a live session closes it.
    ended.store(false, std::sync::atomic::Ordering::SeqCst);
    step2::close_unified(
        &directory,
        TARGET,
        digest,
        connect.context(),
        &record,
        1,
        &ended,
    )
    .unwrap();
    assert!(step1::is_closed(&directory));
}

/// #661 F4: a restart read the record before any sweep was sent; a sweep
/// submitted later in the same session updates it, so no stage can show
/// the unsubmitted close ("No sweep of this split was ever sent") or close
/// without the check. CF: leave the restart's record as read.
#[tokio::test(flavor = "multi_thread")]
async fn a_sweep_submitted_after_restart_is_not_closed_as_unsubmitted() {
    let scan = seed_scan();
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let (digest, directory, _) = fork_only_journal(&scan, &connect, &temp, false);
    let mut panel = resumed(digest, directory, &temp);
    panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
    let port = FakePort::new(connect.context(), 1);
    panel.set_unified_port(Some(port.clone() as Arc<dyn UnifiedPort>));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Unified(UnifiedStage::EnterSeeds));
    assert_eq!(panel.unified().record().map(|r| r.sweep), Some(None));
    assert!(panel.can_confirm_unified_close());

    add_seed(&mut panel, 1, "").await;
    add_seed(&mut panel, 2, "second passphrase").await;
    send(&mut panel, UnifiedMessage::BuildAndSign).await;
    assert_eq!(
        panel.stage(),
        &Stage::Unified(UnifiedStage::Signed),
        "{:?}",
        panel.notice()
    );
    send(&mut panel, UnifiedMessage::Review).await;
    let txid = panel.unified().review().unwrap().txid;
    send(&mut panel, UnifiedMessage::Confirm).await;
    assert_eq!(panel.stage(), &Stage::Unified(UnifiedStage::Submitted));
    assert_eq!(panel.unified().record().map(|r| r.sweep), Some(Some(txid)));
    // Whatever stage a later refusal leaves, the unsubmitted close is gone.
    panel.stage = Stage::Refused(Refusal::retry("refused"));
    assert!(!panel.can_confirm_unified_close());
}

/// #660: the single step's own refusals route their target errors to the
/// sweep's copy, never step 1's or step 2's. CF: route them through the
/// two-step `describe_target`.
#[test]
fn unified_refusals_name_the_sweep_not_a_step() {
    use coincube_core::chain::ChainId;
    for error in [
        TargetError::NotTracking,
        TargetError::NoReservation,
        TargetError::Used(ChainId::Bitcoin),
        TargetError::Used(ChainId::BitcoinBlake2b),
        TargetError::AlreadyReserved,
        TargetError::ReservationUnavailable,
    ] {
        let debug = format!("{error:?}");
        let copy = describe_unified(UnifiedError::Target(error))
            .reason
            .to_lowercase();
        assert!(
            !copy.contains("step 1") && !copy.contains("step 2"),
            "{}: {}",
            debug,
            copy
        );
    }
}

/// #662 F2: a claimed coin spent on BTCB2, found when the single step is
/// opened or restored, reads as the sweep's, never step 1's or step 2's;
/// every other authentication refusal reads as the two-step route's.
/// CF: route it through `step1::evidence_refusal`.
#[test]
fn unified_spent_coin_names_the_sweep() {
    use crate::services::split_evidence::{EvidenceError, EvidenceFailure};
    let spent = evidence_refusal(EvidenceError {
        outpoint: None,
        failure: EvidenceFailure::Btcb2Spent,
    });
    assert_eq!(spent.reason, UNIFIED_COIN_SPENT);
    assert!(!spent.retry);
    let lower = spent.reason.to_lowercase();
    assert!(!lower.contains("step 1") && !lower.contains("step 2"));
    let changed = EvidenceError {
        outpoint: None,
        failure: EvidenceFailure::PostFork,
    };
    assert_eq!(evidence_refusal(changed), step1::evidence_refusal(changed));
}
