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
        absolute::LockTime, bip32::DerivationPath, hashes::Hash, secp256k1::Secp256k1, Network,
        ScriptBuf, WScriptHash,
    },
    signer::SessionSigner,
};

use super::*;
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
    reserves: AtomicUsize,
    builds: AtomicUsize,
    reviews: AtomicUsize,
    confirms: AtomicUsize,
    revoked: AtomicBool,
    /// The fee estimate the review reads (`None`: unavailable).
    review_feerate: Mutex<Option<u64>>,
    /// What a reconcile sees.
    seen: Mutex<Option<TransactionObservation>>,
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
    fn open(&self, open: UnifiedOpen) -> Result<Box<dyn UnifiedFlow>, Step2Refusal> {
        self.calls.opens.lock().unwrap().push(false);
        Ok(Box::new(UnifiedDriver::new(FakeCore {
            open,
            calls: self.calls.clone(),
            target: None,
            sweep: None,
            verified: None,
            reviewed: false,
        })))
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
            route: SubmissionRoute::Connect,
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
    assert!(panel.unified().words().is_empty());
    assert!(panel.unified().passphrase().is_empty());
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
        assert!(!panel.unified().words().is_empty());
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
        assert!(panel.unified().words().is_empty(), "{}", exit);
        assert!(panel.unified().passphrase().is_empty(), "{}", exit);
        assert!(port.calls.revoked.load(Ordering::SeqCst), "{}", exit);
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
    for (file, text) in [("unified.rs", unified), ("view", view.as_str())] {
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
    // No derived Debug on the seed newtype; its own redacts.
    let at = unified
        .find("pub struct SeedText(Zeroizing<String>);")
        .unwrap();
    let attributes = &unified[unified[..at].rfind("\n\n").unwrap()..at];
    assert!(!attributes.contains("Debug"), "{}", attributes);
    assert!(unified.contains("impl fmt::Debug for SeedText"));
    let secret = SeedText::from(mnemonic(1).to_string());
    let printed = format!(
        "{:?} {:?}",
        secret,
        SplitMessage::Unified(UnifiedMessage::Words(secret.clone()))
    );
    assert!(!printed.contains(&*mnemonic(1).to_string()));
    assert!(printed.contains("<redacted>"));
    // Every text input in the view is the seed input, `.secure(true)`, and
    // both seed fields use it.
    assert_eq!(view.matches("text_input(").count(), 1);
    let input = &view[view.find("fn seed_input<").unwrap()..];
    let input = &input[..input.find("\n}\n").unwrap()];
    assert!(input.contains("text_input(placeholder, value.as_str())\n        .secure(true)"));
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
