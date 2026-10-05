//! "Sign with connected device" (#568 B4b-3b) over fake devices: the
//! session-only listing for the policy, the device's output through the
//! verified import seams of step 1 and step 2, the copy per step and per
//! device, stale results, and where the listing comes from (F3).
//!
//! The devices are `split_hardware::bind`'s fakes, which really sign with
//! the fixture wallets' keys; the Ledger rebind goes through a fake opener.
//! Step 1 and step 2 are real core constructions; step 2's preparation is a
//! fake whose checks are the core finalizer's.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
};

use async_hwi::DeviceKind;
use async_trait::async_trait;
use coincube_core::{
    chain::ChainId,
    foreign_split::{
        create_split_step2, finalize_split_step2, FinalizeError, SplitCoin, SplitStep2,
        SplitStep2Inputs,
    },
    miniscript::bitcoin::{
        absolute::LockTime, hashes::Hash, secp256k1::Secp256k1, Psbt, ScriptBuf, WScriptHash,
    },
};
use iced::{futures::StreamExt, Task};

use super::*;
use crate::{
    app::state::vault::split::{
        step1,
        step2::{CannotReplay, FinishRefusal, Step2Coord, Step2Prep, Step2Recovery, Step2Refusal},
    },
    services::{
        claim_workflow::Context,
        split_test_wallets::{self as fixture, Shape},
    },
    split_hardware::{
        bind::tests::{construction, fake, fp, sig_count, temp_root, Fake, FakeOpener},
        sign::{REGISTRATION_NOTICE_ON_DEVICE, REGISTRATION_NOTICE_SESSION},
    },
};

const TARGET: &str = "btcb2-target-cube";

async fn events(task: Task<Message>) -> Vec<SplitEvent> {
    let mut out = Vec::new();
    let Some(mut stream) = iced_runtime::task::into_stream(task) else {
        return out;
    };
    while let Some(action) = stream.next().await {
        if let iced_runtime::Action::Output(Message::Split(event)) = action {
            out.push(*event);
        }
    }
    out
}

async fn drive(panel: &mut SplitPanel, task: Task<Message>) {
    let mut pending = vec![task];
    while let Some(task) = pending.pop() {
        for event in events(task).await {
            pending.push(panel.apply(event));
        }
    }
}

fn supported(id: &str, device: &Fake) -> HardwareWallet {
    HardwareWallet::Supported {
        id: id.to_string(),
        device: device.arc(),
        kind: device.kind,
        fingerprint: fp(device.seed),
        version: None,
        registered: None,
        alias: None,
    }
}

/// The devices that sign `shape`: a Coldcard (key 1) and, for the 2-of-3
/// shapes, a Ledger (key 2), whose rebind goes through `opener`.
fn signers(shape: Shape) -> (Vec<(&'static str, Fake)>, Arc<FakeOpener>) {
    let ledger = fake(DeviceKind::Ledger, 2);
    let mut devices = vec![("coldcard-1", fake(DeviceKind::Coldcard, 1))];
    if matches!(shape, Shape::WshMulti | Shape::WshSortedMulti) {
        devices.push(("ledger-1", ledger.clone()));
    }
    (devices, FakeOpener::new(vec![ledger]))
}

/// A started panel's step-1 sign stage over `shape`'s construction, with
/// the datadir `root` for the listing.
fn step1_panel(shape: Shape, root: &std::path::Path, opener: Arc<FakeOpener>) -> SplitPanel {
    let mut panel = SplitPanel::empty(TARGET.into(), root.join("split-journals"));
    panel.construction = Some(Box::new(construction(shape)));
    panel.stage = Stage::Sign;
    panel.set_device_datadir(CoincubeDirectory::new(root.to_path_buf()));
    panel.set_device_opener(opener);
    panel
}

/// Opens the listing through the panel, then lists `devices` in it as the
/// refresh would.
async fn open(panel: &mut SplitPanel, devices: &[(&str, Fake)]) {
    assert!(panel.can_open_device());
    let task = panel.update(SplitMessage::Device(DeviceMessage::Open));
    assert_eq!(panel.stage, Stage::Working(Work::ListingDevices));
    drive(panel, task).await;
    assert!(panel.device().is_open(), "{:?}", panel.notice());
    let listing = panel.device.listing.as_mut().unwrap();
    for (id, device) in devices {
        listing.devices.list.push(supported(id, device));
    }
}

async fn sign(panel: &mut SplitPanel, id: &str) {
    let task = panel.update(SplitMessage::Device(DeviceMessage::Sign(id.into())));
    assert_eq!(
        panel.stage,
        Stage::Working(Work::SigningOnDevice),
        "{:?}",
        panel.notice()
    );
    drive(panel, task).await;
}

/// A Connect session for the step-2 handoff; nothing here reads a chain.
struct IdleConnect;
#[async_trait]
impl step1::SplitConnect for IdleConnect {
    fn context(&self) -> Context {
        Context {
            generation: 1,
            account: "synthetic-account".into(),
            provider: "synthetic-provider".into(),
        }
    }
    fn evidence(&self) -> &dyn crate::services::split_evidence::SplitEvidenceSource {
        unreachable!("no evidence is read while signing")
    }
    async fn window(&self) -> Result<crate::app::state::vault::claim::ForkWindow, String> {
        unreachable!("no window is read while signing")
    }
    async fn bitcoin_feerate(&self) -> Option<u64> {
        unreachable!("no fee is read while signing")
    }
    async fn address_used(
        &self,
        _: ChainId,
        _: &str,
    ) -> Result<bool, crate::services::claim_observation::FailureKind> {
        unreachable!("no address is read while signing")
    }
    fn open(
        &self,
        _: step1::OpenRequest,
    ) -> Result<Box<dyn step1::Step1Driver>, crate::services::claim_coordinator::Error> {
        unreachable!("step 2 opens no step-1 journal")
    }
}

/// The step-2 preparation's checks are the core finalizer's; `finish`
/// records what it was handed and gives the preparation back.
struct DevicePrep {
    construction: Arc<SplitStep2>,
    verified: Arc<AtomicUsize>,
    finished: Arc<Mutex<Vec<Psbt>>>,
}

#[async_trait]
impl Step2Prep for DevicePrep {
    fn revoke_handle(&self) -> step1::RevokeHandle {
        Arc::new(|| {})
    }
    fn needs_reservation(&self) -> bool {
        false
    }
    async fn check(&mut self, _: &Context) -> Result<CannotReplay, Step2Refusal> {
        unreachable!("the sign stage does not check")
    }
    async fn ensure_target(&mut self, _: &Context) -> Result<u32, Step2Refusal> {
        unreachable!("the sign stage does not reserve")
    }
    async fn build(&mut self, _: &Context, _: Vec<SplitCoin>) -> Result<Psbt, Step2Refusal> {
        unreachable!("the sign stage does not build")
    }
    fn verify_signed(&self, signed: &Psbt, coins: &[SplitCoin]) -> Result<bool, Step2Refusal> {
        self.verified.fetch_add(1, Ordering::SeqCst);
        match finalize_split_step2(
            &self.construction,
            coins,
            self.construction.source(),
            signed,
            &Secp256k1::verification_only(),
        ) {
            Ok(_) => Ok(true),
            Err(FinalizeError::Unsatisfied) => Ok(false),
            Err(error) => Err(Step2Refusal {
                reason: format!("Invalid signed step 2: {error}"),
                retry: true,
                recovery: Step2Recovery::None,
            }),
        }
    }
    fn finish(
        self: Box<Self>,
        _: &Context,
        signed: &Psbt,
        _: &[SplitCoin],
    ) -> Result<Box<dyn Step2Coord>, FinishRefusal> {
        self.finished.lock().unwrap().push(signed.clone());
        Err((
            Step2Refusal {
                reason: "handed over (test)".into(),
                retry: true,
                recovery: Step2Recovery::None,
            },
            Some(self),
        ))
    }
}

struct Step2Fixture {
    panel: SplitPanel,
    construction: Arc<SplitStep2>,
    coins: Vec<SplitCoin>,
    verified: Arc<AtomicUsize>,
    finished: Arc<Mutex<Vec<Psbt>>>,
}

/// A panel in step 2's sign stage: the real step 2 of `shape`'s step 1,
/// built to a P2WSH target over the claimed coins, and a preparation.
fn step2_panel(shape: Shape, root: &std::path::Path, opener: Arc<FakeOpener>) -> Step2Fixture {
    let step1 = construction(shape);
    let wallet = fixture::wallet(shape);
    let inventory = fixture::inventory(&wallet);
    let coins = inventory.splittable_coins();
    let tip = inventory.btcb2_tip_height();
    let target = ScriptBuf::new_p2wsh(&WScriptHash::from_byte_array([9; 32]));
    let construction = Arc::new(
        create_split_step2(
            &SplitStep2Inputs {
                chain: ChainId::BitcoinBlake2b,
                source: step1.source(),
                coins: &coins,
                fork_height: inventory.fork_height(),
                claimed: &step1.claimed_prevouts(),
                target: &target,
            },
            2,
            LockTime::from_height(tip).unwrap(),
            tip,
        )
        .unwrap(),
    );
    let (verified, finished) = (Arc::new(AtomicUsize::new(0)), Arc::default());
    let mut panel = SplitPanel::empty(TARGET.into(), root.join("split-journals"));
    panel.construction = Some(Box::new(step1));
    panel.step2_psbt = Some(construction.psbt().clone());
    panel.coins = coins.clone();
    panel.prep = Some(Box::new(DevicePrep {
        construction: construction.clone(),
        verified: verified.clone(),
        finished: Arc::clone(&finished),
    }));
    panel.stage = Stage::Step2(Step2Stage::Sign);
    panel.connect = Some(Arc::new(IdleConnect));
    panel.set_device_datadir(CoincubeDirectory::new(root.to_path_buf()));
    panel.set_device_opener(opener);
    Step2Fixture {
        panel,
        construction,
        coins,
        verified,
        finished,
    }
}

fn assert_empty(root: &PathBuf) {
    assert_eq!(
        std::fs::read_dir(root).unwrap().count(),
        0,
        "the device listing wrote under the datadir"
    );
}

/// The device's output enters step 1's `step1::import` and step 2's
/// `combine`/`verify_signed` exactly as a signed file: a singlesig device
/// completes step 1; a 2-of-3 needs both devices, the first is partial. In
/// step 2 every device file is checked by the preparation, and the
/// combination of all of them is what the handoff receives. Nothing is
/// written under the datadir.
#[tokio::test(flavor = "multi_thread")]
async fn device_signing_feeds_verified_import_step1_and_step2() {
    for shape in [Shape::Wpkh, Shape::Pkh, Shape::WshSortedMulti] {
        let root = temp_root();
        let (devices, opener) = signers(shape);

        // Step 1.
        let mut panel = step1_panel(shape, &root, opener.clone());
        open(&mut panel, &devices).await;
        for (i, (id, _)) in devices.iter().enumerate() {
            sign(&mut panel, id).await;
            assert_eq!(panel.files(), i + 1, "{shape:?} {id}");
            if i + 1 < devices.len() {
                assert_eq!(panel.stage, Stage::Sign);
                assert!(panel.signed().is_none());
                assert!(panel.notice().unwrap().contains("More are needed"));
                assert!(panel.device().is_open(), "kept for the next device");
            }
        }
        let signed = panel.signed().expect("step 1 finalized through the import");
        let construction = panel.construction().unwrap();
        assert_eq!(
            signed.compute_ntxid(),
            construction.psbt().unsigned_tx.compute_ntxid(),
            "{:?}",
            shape
        );
        assert!(!panel.device().is_open(), "closed once step 1 is signed");
        // Each file is one device's output on the unsigned construction.
        for file in &panel.files {
            assert_eq!(file.unsigned_tx, construction.psbt().unsigned_tx);
            assert_eq!(sig_count(file), construction.psbt().inputs.len());
        }

        // Step 2.
        let Step2Fixture {
            mut panel,
            construction,
            coins,
            verified,
            finished,
        } = step2_panel(shape, &root, opener);
        open(&mut panel, &devices).await;
        for (i, (id, _)) in devices.iter().enumerate() {
            let before = verified.load(Ordering::SeqCst);
            sign(&mut panel, id).await;
            assert!(
                verified.load(Ordering::SeqCst) > before,
                "{:?} {}: checked by the preparation",
                shape,
                id
            );
            assert_eq!(panel.step2_files(), i + 1, "{shape:?} {id}");
            if i + 1 < devices.len() {
                assert!(finished.lock().unwrap().is_empty());
                assert!(panel.notice().unwrap().contains("More are needed"));
            }
        }
        let handed = finished.lock().unwrap().clone();
        assert_eq!(handed.len(), 1, "{:?}", shape);
        assert_eq!(handed[0].unsigned_tx, construction.psbt().unsigned_tx);
        assert_eq!(
            sig_count(&handed[0]),
            devices.len() * construction.psbt().inputs.len(),
            "the combination of every device's file"
        );
        finalize_split_step2(
            &construction,
            &coins,
            construction.source(),
            &handed[0],
            &Secp256k1::verification_only(),
        )
        .unwrap();
        assert_eq!(panel.stage, Stage::Step2(Step2Stage::Sign));
        assert!(panel.prep.is_some());
        assert!(!panel.device().is_open(), "closed once step 2 is signed");

        assert_empty(&root);
        std::fs::remove_dir(&root).unwrap();
    }
}

/// Step 1 shows the data-output note; step 2 shows both "the device says
/// Bitcoin" warnings; a multisig device row adds what its registration
/// means there (session-only on a Ledger, kept on the device otherwise).
/// Only a supported device that holds a key of the wallet can sign.
#[tokio::test(flavor = "multi_thread")]
async fn device_signing_copy_per_step_and_registration_notice() {
    assert_eq!(step_copy(DeviceStep::Step1), &[STEP1_DATA_OUTPUT_NOTE]);
    assert_eq!(
        step_copy(DeviceStep::Step2),
        &[
            DEVICE_SHOWS_BITCOIN_WARNING,
            STEP2_DEVICE_SHOWS_BITCOIN_WARNING
        ]
    );
    let root = temp_root();
    let stranger = fake(DeviceKind::Coldcard, 9);
    let bitbox = fake(DeviceKind::BitBox02, 3);
    for (shape, multisig) in [(Shape::WshMulti, true), (Shape::Wpkh, false)] {
        let (mut devices, opener) = signers(shape);
        devices.push(("stranger", stranger.clone()));
        devices.push(("bitbox-1", bitbox.clone()));
        for step in [DeviceStep::Step1, DeviceStep::Step2] {
            let mut panel = match step {
                DeviceStep::Step1 => step1_panel(shape, &root, opener.clone()),
                DeviceStep::Step2 => step2_panel(shape, &root, opener.clone()).panel,
            };
            assert_eq!(panel.device_step(), Some(step));
            open(&mut panel, &devices).await;
            panel
                .device
                .listing
                .as_mut()
                .unwrap()
                .devices
                .list
                .push(HardwareWallet::Locked {
                    id: "locked-1".into(),
                    device: Arc::new(std::sync::Mutex::new(None)),
                    pairing_code: None,
                    kind: DeviceKind::BitBox02,
                });
            let rows = panel.device().rows();
            let row = |id: &str| {
                rows.iter()
                    .find(|row| row.sign.as_deref() == Some(id))
                    .cloned()
            };
            let coldcard = row("coldcard-1").unwrap();
            assert_eq!(
                coldcard.notice,
                multisig.then_some(REGISTRATION_NOTICE_ON_DEVICE)
            );
            if multisig {
                assert_eq!(
                    row("ledger-1").unwrap().notice,
                    Some(REGISTRATION_NOTICE_SESSION)
                );
            }
            // The BitBox02 holds key 3: one of the 2-of-3's keys, none of
            // the singlesig's.
            assert_eq!(
                row("bitbox-1").map(|r| r.notice),
                multisig.then_some(Some(REGISTRATION_NOTICE_ON_DEVICE))
            );
            // A device with none of the wallet's keys, and a locked one, are
            // listed but not offered.
            assert!(row("stranger").is_none());
            assert!(rows
                .iter()
                .any(|r| r.sign.is_none() && r.label.contains("holds none")));
            assert!(rows
                .iter()
                .any(|r| r.sign.is_none() && r.label.contains("unlock the device")));
            assert_eq!(
                rows.iter().filter(|r| r.sign.is_some()).count(),
                if multisig { 3 } else { 1 }
            );

            // What the view renders: the step's copy, each notice once.
            let labels = rendered_labels(&panel).await;
            for line in step_copy(step) {
                assert_eq!(
                    labels.iter().filter(|l| l == line).count(),
                    1,
                    "{step:?}: {line}"
                );
            }
            let other = match step {
                DeviceStep::Step1 => STEP2_DEVICE_SHOWS_BITCOIN_WARNING,
                DeviceStep::Step2 => STEP1_DATA_OUTPUT_NOTE,
            };
            assert!(!labels.iter().any(|l| l == other), "{:?}", step);
            for (notice, rows) in [
                (REGISTRATION_NOTICE_SESSION, 1),
                (REGISTRATION_NOTICE_ON_DEVICE, 2),
            ] {
                let shown = labels.iter().filter(|l| *l == notice).count();
                assert_eq!(shown, if multisig { rows } else { 0 }, "{step:?} {notice}");
            }
        }
    }
    assert_empty(&root);
    std::fs::remove_dir(&root).unwrap();
}

/// F3: the panel's listing is `HardwareWallets::new(datadir, Bitcoin)` bound
/// with `with_split_policy` to the policy being signed, and never a loaded
/// wallet's listing. It persists no pairing and writes nothing under the
/// datadir.
#[tokio::test(flavor = "multi_thread")]
async fn split_listing_is_built_from_with_split_policy_only() {
    assert_listing_source_uses_split_policy_only(include_str!("../device.rs"));

    let root = temp_root();
    for shape in [Shape::Wpkh, Shape::WshSortedMulti] {
        let (devices, opener) = signers(shape);
        let mut panel = step1_panel(shape, &root, opener);
        open(&mut panel, &devices).await;
        let policy = split_policy(panel.construction().unwrap().source()).unwrap();
        let descriptor = split_descriptor(panel.construction().unwrap().source())
            .unwrap()
            .to_string();
        let listing = &panel.device.listing.as_ref().unwrap().devices;
        assert!(!listing.persists_pairing());
        let binding = listing.split_policy_binding().expect("bound to the policy");
        assert_eq!(binding.name(), policy.name());
        if policy.shape().is_multisig() {
            assert_eq!(binding.bound(), Some((policy.name(), descriptor.as_str())));
        }
        for (id, _) in &devices {
            sign(&mut panel, id).await;
        }
        assert!(panel.signed().is_some());
    }
    assert_empty(&root);
    std::fs::remove_dir(&root).unwrap();
}

/// The source half of F3 above: `device.rs`'s production text builds its
/// listing only through `with_split_policy`.
fn assert_listing_source_uses_split_policy_only(text: &str) {
    let text = crate::utils::source_text::lf_only(text);
    let production = &text[..text.find("\n#[cfg(all(test, unix))]\n").unwrap()];
    assert!(!production.contains("with_wallet("));
    assert!(!production.contains("ephemeral("));
    assert_eq!(production.matches("HardwareWallets::new(").count(), 1);
    let build = &production[production.find("pub fn build_listing(").unwrap()..];
    let build = &build[..build.find("\n}\n").unwrap()];
    assert!(build.contains("HardwareWallets::new(datadir, Network::Bitcoin)"));
    assert!(build.contains(".with_split_policy(policy.name().to_string(), descriptor)"));
}

/// #568 W1: the F3 source guard reads a CRLF checkout (Git for Windows) of
/// `device.rs` the same way, so its line-spanning markers are still found.
#[test]
fn listing_source_guard_reads_a_crlf_checkout() {
    let crlf = crate::utils::source_text::as_crlf(include_str!("../device.rs"));
    assert!(crlf.contains("\r\n#[cfg(all(test, unix))]\r\n"));
    assert_listing_source_uses_split_policy_only(&crlf);
}

/// A device result from a request the panel moved on from is dropped: a
/// revocation (session end, Cube close) while the device works, or a close.
/// Nothing is imported, and the stage is not moved by the late result.
#[tokio::test(flavor = "multi_thread")]
async fn stale_device_result_is_dropped() {
    let root = temp_root();
    for close in [false, true] {
        let (devices, opener) = signers(Shape::Wpkh);
        let mut panel = step1_panel(Shape::Wpkh, &root, opener);
        open(&mut panel, &devices).await;
        let task = panel.update(SplitMessage::Device(DeviceMessage::Sign(
            "coldcard-1".into(),
        )));
        assert_eq!(panel.stage, Stage::Working(Work::SigningOnDevice));
        // No other device request while one works.
        let idle = panel.update(SplitMessage::Device(DeviceMessage::Sign(
            "coldcard-1".into(),
        )));
        assert!(events(idle).await.is_empty());
        if close {
            let _ = panel.update(SplitMessage::Close);
            assert!(panel.is_hidden());
        } else {
            panel.revoke();
        }
        assert!(!panel.device().is_open());
        let stage = panel.stage.clone();
        let late = events(task).await;
        assert!(matches!(
            late.as_slice(),
            [SplitEvent::DeviceSigned(_, Ok(_))]
        ));
        for event in late {
            let task = panel.apply(event);
            drive(&mut panel, task).await;
        }
        assert_eq!(panel.files(), 0, "close={close}");
        assert!(panel.signed().is_none());
        assert_eq!(panel.stage, stage);
    }
    // A listing that arrives after a revocation is dropped too.
    let (_, opener) = signers(Shape::Wpkh);
    let mut panel = step1_panel(Shape::Wpkh, &root, opener);
    let task = panel.update(SplitMessage::Device(DeviceMessage::Open));
    panel.revoke();
    for event in events(task).await {
        drop(panel.apply(event));
    }
    assert!(!panel.device().is_open());
    assert_empty(&root);
    std::fs::remove_dir(&root).unwrap();
}

/// #653 F3: Cancel is refused while a device signs. The listing stays, and
/// the device's result still lands in the import.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_is_refused_while_a_device_signs() {
    let root = temp_root();
    let (devices, opener) = signers(Shape::Wpkh);
    let mut panel = step1_panel(Shape::Wpkh, &root, opener);
    open(&mut panel, &devices).await;
    let task = panel.update(SplitMessage::Device(DeviceMessage::Sign(
        "coldcard-1".into(),
    )));
    assert_eq!(panel.stage, Stage::Working(Work::SigningOnDevice));
    let cancelled = panel.update(SplitMessage::Device(DeviceMessage::Cancel));
    assert!(events(cancelled).await.is_empty());
    assert!(panel.device().is_open());
    assert_eq!(panel.device().step(), Some(DeviceStep::Step1));
    assert_eq!(panel.stage, Stage::Working(Work::SigningOnDevice));
    drive(&mut panel, task).await;
    assert_eq!(panel.files(), 1);
    assert!(panel.signed().is_some(), "{:?}", panel.notice());
    // Once nothing works, Cancel closes the listing.
    let mut panel = step1_panel(Shape::Wpkh, &root, signers(Shape::Wpkh).1);
    open(&mut panel, &devices).await;
    let _ = panel.update(SplitMessage::Device(DeviceMessage::Cancel));
    assert!(!panel.device().is_open());
    assert_eq!(panel.stage, Stage::Sign);
    assert_empty(&root);
    std::fs::remove_dir(&root).unwrap();
}

/// #653 F3: a device that fails (here a Ledger the opener can no longer
/// reach) shows why, back in the sign stage, with nothing imported.
#[tokio::test(flavor = "multi_thread")]
async fn device_refusal_shows_its_reason() {
    let root = temp_root();
    let ledger = fake(DeviceKind::Ledger, 1);
    let mut panel = step1_panel(Shape::Wpkh, &root, FakeOpener::new(Vec::new()));
    open(&mut panel, &[("ledger-1", ledger)]).await;
    sign(&mut panel, "ledger-1").await;
    assert_eq!(panel.stage, Stage::Sign);
    assert_eq!(panel.files(), 0);
    assert!(panel.signed().is_none());
    let notice = panel.notice().expect("the device's refusal is shown");
    assert!(notice.starts_with("Device error"), "{}", notice);
    assert!(panel.device().is_open(), "another device may still sign");
    assert_empty(&root);
    std::fs::remove_dir(&root).unwrap();
}

/// The step-1 sign stage's view, as rendered.
pub(in crate::app::state::vault::split) async fn rendered_labels(
    panel: &SplitPanel,
) -> Vec<String> {
    use iced::advanced::{
        layout,
        renderer::Headless,
        widget::{Id, Operation, Tree},
        Layout,
    };
    #[derive(Default)]
    struct Labels(Vec<String>);
    impl Operation for Labels {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }
        fn text(&mut self, _: Option<&Id>, _: iced::Rectangle, text: &str) {
            self.0.push(text.to_owned());
        }
    }
    let renderer = <iced::Renderer as Headless>::new(
        iced::Font::DEFAULT,
        iced::Pixels(16.0),
        Some("tiny-skia"),
    )
    .await
    .expect("software renderer for the Split view");
    let mut element = crate::app::view::vault::split::split_panel(panel);
    let mut tree = Tree::new(element.as_widget());
    let node = element.as_widget_mut().layout(
        &mut tree,
        &renderer,
        &layout::Limits::new(iced::Size::ZERO, iced::Size::new(1200.0, 2400.0)),
    );
    let mut labels = Labels::default();
    element
        .as_widget_mut()
        .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
    labels.0
}

/// The step-1 construction of `wallet` (as `bind::tests::construction`).
fn construction_of(wallet: &fixture::Wallet) -> coincube_core::foreign_split::SplitStep1 {
    use crate::services::{foreign_split_inventory::FreshIndex, split_source::split_source};
    use coincube_core::{
        foreign_split::{create_split_step1, SplitInputs},
        miniscript::bitcoin::BlockHash,
    };
    let inventory = fixture::inventory(wallet);
    let coins = inventory.splittable_coins();
    let source = split_source(&wallet.external, Some(&wallet.internal)).unwrap();
    let FreshIndex::Proven(destination) = inventory.fresh_receive() else {
        panic!("fixture has a fresh index");
    };
    let tip = inventory.bitcoin_tip_height();
    create_split_step1(
        &SplitInputs {
            chain: ChainId::Bitcoin,
            source: &source,
            coins: &coins,
            fork_height: inventory.fork_height(),
            destination,
        },
        2,
        LockTime::from_height(tip).unwrap(),
        tip,
        BlockHash::from_byte_array([7; 32]),
    )
    .unwrap()
}

/// #653 F4: "Sign with connected device" is offered only for a wallet whose
/// descriptors give the in-app hardware route (`SigningRoutes`, U6). A
/// wallet whose keys carry no origin gets no listing, and the view offers
/// none; the same shape with origins does. CF: drop the route conjunct
/// from `can_open_device`.
#[tokio::test(flavor = "multi_thread")]
async fn device_listing_needs_the_in_app_hardware_route() {
    use crate::services::foreign_scan::{Branch, ScanDescriptor};
    use coincube_core::miniscript::bitcoin::bip32::{DerivationPath, Xpub};
    use std::str::FromStr;
    let root = temp_root();
    let secp = Secp256k1::new();
    let master = fixture::master(1);
    let account = master
        .derive_priv(&secp, &DerivationPath::from_str("m/84'/0'/0'").unwrap())
        .unwrap();
    let xpub = Xpub::from_priv(&secp, &account);
    let parse = |branch, step: u32| {
        ScanDescriptor::parse(branch, &format!("wpkh({}/{}/*)", xpub, step)).unwrap()
    };
    let bare = fixture::Wallet {
        external: parse(Branch::External, 0),
        internal: parse(Branch::Internal, 1),
        signers: vec![master],
    };
    let (_, opener) = signers(Shape::Wpkh);
    let mut panel = step1_panel(Shape::Wpkh, &root, opener);
    assert!(panel.can_open_device());
    panel.construction = Some(Box::new(construction_of(&bare)));
    let routes = signing_routes(panel.construction().unwrap().source());
    assert!(routes.psbt_file && !routes.in_app_hardware && !routes.seed_unified);
    assert!(!panel.can_open_device());
    let task = panel.update(SplitMessage::Device(DeviceMessage::Open));
    assert!(events(task).await.is_empty());
    assert_eq!(panel.stage, Stage::Sign);
    assert!(!panel.device().is_open());
    let labels = rendered_labels(&panel).await;
    assert!(!labels.iter().any(|l| l == "Sign with connected device"));
    assert!(labels.iter().any(|l| l == "Import signed"));
    assert_empty(&root);
    std::fs::remove_dir(&root).unwrap();
}

/// #653 N1: the listing's refresh runs only while the panel is shown.
/// CF: drop `!self.hidden` from `subscription`.
#[tokio::test(flavor = "multi_thread")]
async fn device_listing_refresh_stops_while_hidden() {
    let root = temp_root();
    let (devices, opener) = signers(Shape::Wpkh);
    let mut panel = step1_panel(Shape::Wpkh, &root, opener);
    open(&mut panel, &devices).await;
    let recipes =
        |panel: &SplitPanel| iced::advanced::subscription::into_recipes(panel.subscription()).len();
    assert_eq!(recipes(&panel), 1);
    panel.hidden = true;
    assert_eq!(recipes(&panel), 0);
    panel.hidden = false;
    assert_eq!(recipes(&panel), 1);
    assert_empty(&root);
    std::fs::remove_dir(&root).unwrap();
}

/// #653 N2: the Sign arm signs only for the step on screen and the listing
/// opened for it: nothing happens outside a sign stage, nor with a listing
/// opened for the other step. CF: drop the listing's step check.
#[tokio::test(flavor = "multi_thread")]
async fn device_sign_needs_the_step_its_listing_was_opened_for() {
    let root = temp_root();
    let (devices, opener) = signers(Shape::Wpkh);
    let mut panel = step1_panel(Shape::Wpkh, &root, opener);
    open(&mut panel, &devices).await;
    // A listing opened for the other step.
    panel.device.step = Some(DeviceStep::Step2);
    let task = panel.update(SplitMessage::Device(DeviceMessage::Sign(
        "coldcard-1".into(),
    )));
    assert!(events(task).await.is_empty());
    assert_eq!(panel.stage, Stage::Sign);
    // No sign stage on screen.
    panel.device.step = Some(DeviceStep::Step1);
    panel.stage = Stage::Ready;
    let task = panel.update(SplitMessage::Device(DeviceMessage::Sign(
        "coldcard-1".into(),
    )));
    assert!(events(task).await.is_empty());
    assert_eq!(panel.stage, Stage::Ready);
    assert_eq!(panel.files(), 0);
    // Back in the sign stage it signs.
    panel.stage = Stage::Sign;
    sign(&mut panel, "coldcard-1").await;
    assert_eq!(panel.files(), 1);
    assert_empty(&root);
    std::fs::remove_dir(&root).unwrap();
}

/// #653 N3: an open listing is closed by "Close device list", which then
/// offers "Sign with connected device" again. CF: drop that action from
/// the view.
#[tokio::test(flavor = "multi_thread")]
async fn close_device_list_closes_the_listing() {
    let root = temp_root();
    let (devices, opener) = signers(Shape::Wpkh);
    let mut panel = step1_panel(Shape::Wpkh, &root, opener);
    let labels = rendered_labels(&panel).await;
    assert!(labels.iter().any(|l| l == "Sign with connected device"));
    assert!(!labels.iter().any(|l| l == "Close device list"));
    open(&mut panel, &devices).await;
    let labels = rendered_labels(&panel).await;
    assert!(
        labels.iter().any(|l| l == "Close device list"),
        "{:?}",
        labels
    );
    assert!(!labels.iter().any(|l| l == "Sign with connected device"));
    let _ = panel.update(SplitMessage::Device(DeviceMessage::Cancel));
    assert!(!panel.device().is_open());
    let labels = rendered_labels(&panel).await;
    assert!(labels.iter().any(|l| l == "Sign with connected device"));
    assert_empty(&root);
    std::fs::remove_dir(&root).unwrap();
}

/// #653 N4: Cancel while the listing is being built is refused, so the
/// listing lands and the panel returns to its sign stage; a cancel there
/// would drop the step and leave the panel working. CF: admit Cancel during
/// `ListingDevices`.
#[tokio::test(flavor = "multi_thread")]
async fn cancel_is_refused_while_the_listing_is_built() {
    let root = temp_root();
    let (_, opener) = signers(Shape::Wpkh);
    let mut panel = step1_panel(Shape::Wpkh, &root, opener);
    let task = panel.update(SplitMessage::Device(DeviceMessage::Open));
    assert_eq!(panel.stage, Stage::Working(Work::ListingDevices));
    let cancelled = panel.update(SplitMessage::Device(DeviceMessage::Cancel));
    assert!(events(cancelled).await.is_empty());
    assert_eq!(panel.device().step(), Some(DeviceStep::Step1));
    drive(&mut panel, task).await;
    assert_eq!(panel.stage, Stage::Sign);
    assert!(panel.device().is_open());
    assert_empty(&root);
    std::fs::remove_dir(&root).unwrap();
}
