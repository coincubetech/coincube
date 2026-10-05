//! Split step-1 panel (#568 B1b) state tests against a mocked Connect.
//!
//! [`FakeConnect`] stands in for Connect: the anchor window, the Bitcoin fee,
//! fresh address-history reads and the per-outpoint chain evidence are
//! synthetic. The journal is real: the fake opens it with the same
//! `claim_workflow::Controller` Split API the coordinator uses, in a private
//! temporary directory, so file permissions, contents and restart are the
//! production ones. The coordinator's own review, preflight and submission
//! are B0b's (`claim_coordinator::split::tests`); [`FakeDriver`] records what
//! the panel asks of it. Signatures come from rust-bitcoin's PSBT signer and
//! travel through real PSBT files.

use super::*;
use crate::services::{
    claim_coordinator,
    claim_observation::{FailureKind, FreshRead, TransactionObservation},
    claim_preflight::NodePolicy,
    claim_workflow::{Context, Controller},
    coincube::CoincubeClient,
    foreign_scan::ScanReport,
    foreign_split_inventory::{SplitInventory, TwoChainScan},
    split_evidence::SplitEvidenceSource,
    split_test_wallets::{self as fixture, Shape},
};
use async_trait::async_trait;
use coincube_core::{
    chain::ChainId,
    claim::{Assessment, BlockRef},
    miniscript::bitcoin::{
        absolute::LockTime, hashes::Hash, secp256k1::Secp256k1, Address, BlockHash, Network,
        OutPoint,
    },
};
use iced::futures::StreamExt;
use reqwest::header::{HeaderMap, CACHE_CONTROL};
use std::{
    collections::{BTreeSet, HashMap},
    os::unix::fs::PermissionsExt,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
};
use step1::{Recovery, ReviewView, SplitConnect};

const TARGET: &str = "btcb2-target-cube";

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn fresh<T>(chain: ChainId, value: T) -> FreshRead<T> {
    read_at(chain, value, now())
}

fn read_at<T>(chain: ChainId, value: T, observed_at: i64) -> FreshRead<T> {
    let mut headers = HeaderMap::new();
    headers.insert("x-cache", "BYPASS".parse().unwrap());
    headers.insert(CACHE_CONTROL, "no-store".parse().unwrap());
    FreshRead::from_response(chain, value, observed_at, &headers).unwrap()
}

/// An injected Bitcoin read fault (#626 F1): the read fails, or answers with
/// a stamp older than any freshness bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    Error,
    Stale,
}

fn faulted<T>(fault: Option<Fault>, chain: ChainId, value: T) -> Result<FreshRead<T>, FailureKind> {
    match fault {
        None => Ok(fresh(chain, value)),
        Some(Fault::Error) => Err(FailureKind::Http(503)),
        Some(Fault::Stale) => Ok(read_at(chain, value, now() - 3_600)),
    }
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "split-panel-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
    /// `<datadir>/…/split` stand-in.
    fn root(&self) -> PathBuf {
        self.0.join("split")
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Both chains as Connect would show them: every fixture coin confirmed in
/// the same pre-fork block on both, unspent on both.
struct Chains {
    previous: HashMap<Txid, Transaction>,
    status: Mutex<HashMap<(ChainId, Txid), TransactionObservation>>,
    canonical: HashMap<(ChainId, u64), BlockHash>,
    utxos: Mutex<HashMap<(ChainId, String), BTreeSet<OutPoint>>>,
    /// Faults on Bitcoin transaction-status and unspent-output reads.
    status_fault: Mutex<Option<Fault>>,
    utxo_fault: Mutex<Option<Fault>>,
}

impl Chains {
    fn of(coins: &[crate::services::foreign_scan::DiscoveredCoin]) -> Self {
        let mut chains = Self {
            previous: HashMap::new(),
            status: Mutex::new(HashMap::new()),
            canonical: HashMap::new(),
            utxos: Mutex::new(HashMap::new()),
            status_fault: Mutex::new(None),
            utxo_fault: Mutex::new(None),
        };
        for coin in coins {
            let block = BlockRef {
                height: u64::from(coin.block_height.unwrap()),
                hash: coin.block_hash.unwrap(),
            };
            chains
                .previous
                .insert(coin.outpoint.txid, coin.previous.clone());
            let address = Address::from_script(&coin.output.script_pubkey, Network::Bitcoin)
                .unwrap()
                .to_string();
            for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
                chains.status.lock().unwrap().insert(
                    (chain, coin.outpoint.txid),
                    TransactionObservation::Confirmed {
                        txid: coin.outpoint.txid,
                        block,
                    },
                );
                chains.canonical.insert((chain, block.height), block.hash);
                chains
                    .utxos
                    .lock()
                    .unwrap()
                    .entry((chain, address.clone()))
                    .or_default()
                    .insert(coin.outpoint);
            }
        }
        chains
    }
    fn spend_on(&self, chain: ChainId, outpoint: OutPoint) {
        for (key, set) in self.utxos.lock().unwrap().iter_mut() {
            if key.0 == chain {
                set.remove(&outpoint);
            }
        }
    }
}

#[async_trait]
impl SplitEvidenceSource for Chains {
    fn now(&self) -> i64 {
        now()
    }
    async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        let height = match chain {
            ChainId::Bitcoin => fixture::BITCOIN_TIP_HEIGHT,
            _ => fixture::BTCB2_TIP_HEIGHT,
        };
        Ok(fresh(
            chain,
            BlockRef {
                height: u64::from(height),
                hash: fixture::block_hash(u64::from(height)),
            },
        ))
    }
    async fn transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        let value = self
            .status
            .lock()
            .unwrap()
            .get(&(chain, txid))
            .copied()
            .unwrap_or(TransactionObservation::Absent);
        let fault = (chain == ChainId::Bitcoin)
            .then(|| *self.status_fault.lock().unwrap())
            .flatten();
        faulted(fault, chain, value)
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        self.canonical
            .get(&(chain, height))
            .map(|hash| fresh(chain, *hash))
            .ok_or(FailureKind::Http(404))
    }
    async fn previous_transaction(
        &self,
        _chain: ChainId,
        txid: Txid,
    ) -> Result<Transaction, FailureKind> {
        self.previous
            .get(&txid)
            .cloned()
            .ok_or(FailureKind::Http(404))
    }
    async fn unspent_outputs(
        &self,
        chain: ChainId,
        address: &str,
    ) -> Result<FreshRead<Vec<OutPoint>>, FailureKind> {
        let set = self
            .utxos
            .lock()
            .unwrap()
            .get(&(chain, address.to_owned()))
            .cloned()
            .unwrap_or_default();
        let fault = (chain == ChainId::Bitcoin)
            .then(|| *self.utxo_fault.lock().unwrap())
            .flatten();
        faulted(fault, chain, set.into_iter().collect())
    }
}

/// What the panel asked of the coordinator.
#[derive(Default)]
struct Calls {
    opened: Mutex<Vec<bool>>,
    reviews: AtomicUsize,
    submits: AtomicUsize,
    reconciles: AtomicUsize,
    revoked: AtomicBool,
    address_reads: Mutex<Vec<(ChainId, String)>>,
    /// What the next reconcile reports (default Unchecked).
    status: Mutex<Option<Status>>,
    /// What the next reorg review finds (default: nothing to review).
    recovery: Mutex<Option<Recovery>>,
    recovers: AtomicUsize,
    acknowledges: AtomicUsize,
    resends: AtomicUsize,
}

#[derive(Clone)]
enum SubmitPlan {
    Accept,
    Refuse(String),
}

struct FakeConnect {
    context: Context,
    window: Mutex<Result<ForkWindow, String>>,
    feerate: Option<u64>,
    used: Mutex<HashMap<ChainId, Result<bool, FailureKind>>>,
    chains: Chains,
    calls: Arc<Calls>,
    submit: Mutex<SubmitPlan>,
    /// A fresh `open` refuses, before or after writing the journal.
    refuse_open: Mutex<Option<bool>>,
}

use crate::app::state::vault::claim::ForkWindow;

fn window() -> ForkWindow {
    let mtp = now();
    ForkWindow {
        fork_height: fixture::FORK,
        fork_hash: fixture::block_hash(fixture::FORK),
        tip_height: u64::from(fixture::BTCB2_TIP_HEIGHT),
        median_time_past: mtp,
        expires_at: mtp + 30 * 24 * 3600,
        rdts: Ok(()),
    }
}

impl FakeConnect {
    fn new(coins: &[crate::services::foreign_scan::DiscoveredCoin]) -> Arc<Self> {
        Arc::new(Self {
            context: Context {
                generation: 0,
                account: "synthetic-account".into(),
                provider: "synthetic-provider".into(),
            },
            window: Mutex::new(Ok(window())),
            feerate: Some(3),
            used: Mutex::new(HashMap::new()),
            chains: Chains::of(coins),
            calls: Arc::default(),
            submit: Mutex::new(SubmitPlan::Accept),
            refuse_open: Mutex::new(None),
        })
    }
}

#[async_trait]
impl SplitConnect for FakeConnect {
    fn context(&self) -> Context {
        self.context.clone()
    }
    fn evidence(&self) -> &dyn SplitEvidenceSource {
        &self.chains
    }
    async fn window(&self) -> Result<ForkWindow, String> {
        self.window.lock().unwrap().clone()
    }
    async fn bitcoin_feerate(&self) -> Option<u64> {
        self.feerate
    }
    async fn address_used(&self, chain: ChainId, address: &str) -> Result<bool, FailureKind> {
        self.calls
            .address_reads
            .lock()
            .unwrap()
            .push((chain, address.to_owned()));
        self.used
            .lock()
            .unwrap()
            .get(&chain)
            .copied()
            .unwrap_or(Ok(false))
    }
    fn open(&self, request: OpenRequest) -> Result<Box<dyn Step1Driver>, claim_coordinator::Error> {
        self.calls.opened.lock().unwrap().push(request.resume);
        let context = self.context.clone();
        let mut controller = if request.resume {
            let identity = crate::services::claim_workflow::split_identity(
                request.target_cube.clone(),
                request.construction.source().digest(),
            );
            Controller::reopen(&request.directory, &identity, context.clone())?
        } else {
            let refuse = *self.refuse_open.lock().unwrap();
            if refuse == Some(false) {
                return Err(claim_coordinator::Error::InvalidBinding);
            }
            crate::services::claim_workflow::prepare_directory(&request.directory)?;
            let controller = Controller::create_split(
                &request.directory,
                request.target_cube.clone(),
                &request.construction,
                &request.verified,
                request.fork_height,
                context.clone(),
            )?;
            if refuse == Some(true) {
                return Err(claim_coordinator::Error::InvalidBinding);
            }
            controller
        };
        controller.revalidate_split_construction(
            &context,
            &request.construction,
            request.fork_height,
        )?;
        controller.bind_recovered_split_transaction(&context, &request.verified)?;
        Ok(Box::new(FakeDriver {
            phase: controller.phase(),
            _controller: controller,
            calls: self.calls.clone(),
            plan: self.submit.lock().unwrap().clone(),
            reviewed: false,
            recovered: None,
            txid: request.verified.transaction().compute_txid(),
        }))
    }
}

struct FakeDriver {
    /// Holds the journal lock like the coordinator does.
    _controller: Controller,
    phase: Phase,
    calls: Arc<Calls>,
    plan: SubmitPlan,
    reviewed: bool,
    recovered: Option<Recovery>,
    txid: Txid,
}

#[async_trait]
impl Step1Driver for FakeDriver {
    fn phase(&self) -> Phase {
        self.phase
    }
    fn revoke_handle(&self) -> RevokeHandle {
        let calls = self.calls.clone();
        Arc::new(move || calls.revoked.store(true, Ordering::SeqCst))
    }
    async fn review(&mut self, _: &Context) -> Result<ReviewView, claim_coordinator::Error> {
        self.calls.reviews.fetch_add(1, Ordering::SeqCst);
        self.reviewed = true;
        Ok(ReviewView {
            txid: self.txid,
            fee_sats: 1_000,
            vsize: 300,
            route: claim_coordinator::SubmissionRoute::Connect,
            bitcoin_tip: u64::from(fixture::BITCOIN_TIP_HEIGHT),
            fork_tip: u64::from(fixture::BTCB2_TIP_HEIGHT),
            rdts_left: Some(30 * 24 * 3600),
        })
    }
    async fn submit(&mut self, _: &Context) -> Result<Outcome, claim_coordinator::Error> {
        self.calls.submits.fetch_add(1, Ordering::SeqCst);
        if !std::mem::take(&mut self.reviewed) {
            return Err(claim_coordinator::Error::InvalidReview);
        }
        match &self.plan {
            SubmitPlan::Accept => {
                self.phase = Phase::BroadcastUncertain;
                Ok(Outcome::Uncertain {
                    txid: self.txid,
                    wtxid: coincube_core::miniscript::bitcoin::Wtxid::all_zeros(),
                })
            }
            SubmitPlan::Refuse(reason) => Err(claim_coordinator::Error::PolicyRejected(
                NodePolicy::Rejected {
                    reason: reason.clone(),
                },
            )),
        }
    }
    async fn reconcile(&mut self, _: &Context) -> Result<Status, claim_coordinator::Error> {
        self.calls.reconciles.fetch_add(1, Ordering::SeqCst);
        self.recovered = None;
        Ok(self
            .calls
            .status
            .lock()
            .unwrap()
            .unwrap_or(Status::Unchecked))
    }
    async fn recover(&mut self, _: &Context) -> Result<Recovery, claim_coordinator::Error> {
        self.calls.recovers.fetch_add(1, Ordering::SeqCst);
        self.recovered = self.calls.recovery.lock().unwrap().clone();
        self.recovered
            .clone()
            .ok_or(claim_coordinator::Error::NotReady(Assessment::Reorged))
    }
    async fn acknowledge(&mut self, _: &Context) -> Result<(), claim_coordinator::Error> {
        self.calls.acknowledges.fetch_add(1, Ordering::SeqCst);
        match self.recovered.take() {
            Some(Recovery::Reconfirmed { .. }) => Ok(()),
            _ => Err(claim_coordinator::Error::InvalidReview),
        }
    }
    async fn resend(&mut self, _: &Context) -> Result<Outcome, claim_coordinator::Error> {
        self.calls.resends.fetch_add(1, Ordering::SeqCst);
        match self.recovered.take() {
            Some(Recovery::Resend(_)) => Ok(Outcome::Uncertain {
                txid: self.txid,
                wtxid: coincube_core::miniscript::bitcoin::Wtxid::all_zeros(),
            }),
            _ => Err(claim_coordinator::Error::InvalidReview),
        }
    }
}

/// A scanned foreign wallet handed over from Home.
struct Scan {
    wallet: fixture::Wallet,
    coins: Vec<crate::services::foreign_scan::DiscoveredCoin>,
}

impl Scan {
    fn new(shape: Shape) -> Self {
        let wallet = fixture::wallet(shape);
        let coins = fixture::shared_coins(&wallet);
        Self { wallet, coins }
    }
    fn intent_with(&self, edit: impl Fn(ScanReport) -> ScanReport) -> SplitIntent {
        let btcb2 = edit(fixture::report(ChainId::BitcoinBlake2b, self.coins.clone()));
        let bitcoin = edit(fixture::report(ChainId::Bitcoin, self.coins.clone()));
        let inventory = SplitInventory::join(&btcb2, &bitcoin, fixture::GENERATION, true).unwrap();
        let mut client = CoincubeClient::new();
        client.set_token("synthetic-test-token");
        SplitIntent::new(
            TARGET.into(),
            ChainId::BitcoinBlake2b,
            0,
            &client,
            TwoChainScan {
                btcb2,
                bitcoin,
                inventory,
            },
            self.wallet.external.clone(),
            Some(self.wallet.internal.clone()),
        )
        .unwrap()
    }
    fn intent(&self) -> SplitIntent {
        self.intent_with(|report| report)
    }
}

/// Run a task the panel returned; collect the Split events it produced.
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

/// Apply every event and the tasks they start until the panel settles.
async fn drive(panel: &mut SplitPanel, task: Task<Message>) {
    let mut pending = vec![task];
    while let Some(task) = pending.pop() {
        for event in events(task).await {
            pending.push(panel.apply(event));
        }
    }
}

fn sign_to_file(
    panel: &SplitPanel,
    signers: &[coincube_core::miniscript::bitcoin::bip32::Xpriv],
    dir: &Path,
    name: &str,
) -> PathBuf {
    let secp = Secp256k1::new();
    let mut psbt = panel.construction().unwrap().psbt().clone();
    for signer in signers {
        psbt.sign(signer, &secp).unwrap();
    }
    let path = dir.join(name);
    std::fs::write(&path, split_psbt_file::encode(&psbt, Encoding::Base64)).unwrap();
    path
}

use std::path::Path;

/// Fresh flow up to a recorded, unsubmitted step 1.
async fn recorded(scan: &Scan, connect: &Arc<FakeConnect>, temp: &Temp) -> SplitPanel {
    let mut panel = SplitPanel::start(TARGET.into(), temp.root(), scan.intent());
    panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Sign, "{:?}", panel.stage());

    // Export the unsigned PSBT file; it is exactly the construction.
    let exported = temp.0.join("unsigned.psbt");
    let task = panel.export_to(exported.clone(), Encoding::Binary);
    drive(&mut panel, task).await;
    assert_eq!(panel.exported(), Some(&exported));
    assert_eq!(
        split_psbt_file::load(&exported).unwrap(),
        *panel.construction().unwrap().psbt()
    );

    // Each cosigner returns a file; import and combine until satisfied.
    let files: Vec<PathBuf> = scan
        .wallet
        .signers
        .iter()
        .enumerate()
        .map(|(i, signer)| sign_to_file(&panel, &[*signer], &temp.0, &format!("signed-{i}.txt")))
        .collect();
    let (first, rest) = files.split_first().unwrap();
    let task = panel.import_from(vec![first.clone()]);
    drive(&mut panel, task).await;
    if rest.is_empty() {
        assert_eq!(panel.stage(), &Stage::Ready);
    } else {
        assert_eq!(panel.stage(), &Stage::Sign);
        assert!(panel.notice().unwrap().contains("More are needed"));
        assert!(panel.signed().is_none());
        let task = panel.import_from(rest.to_vec());
        drive(&mut panel, task).await;
    }
    assert_eq!(panel.stage(), &Stage::Ready, "{:?}", panel.notice());
    assert_eq!(panel.phase(), Some(Phase::Intent));
    assert!(panel.is_bound());
    panel
}

/// #625 F3b: a recording the coordinator refused after writing the journal
/// continues from that journal, never from a second record; the task finds
/// it, off the UI thread, and the event carries it. With nothing written
/// (and no journal root on disk) the event carries none.
#[tokio::test(flavor = "multi_thread")]
async fn recorded_refusal_carries_the_discovered_journal() {
    for written in [true, false] {
        let scan = Scan::new(Shape::Wpkh);
        let connect = FakeConnect::new(&scan.coins);
        *connect.refuse_open.lock().unwrap() = Some(written);
        let temp = Temp::new();
        let root = temp.root().join("absent-split-root");
        let mut panel = SplitPanel::start(TARGET.into(), root.clone(), scan.intent());
        panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
        let task = panel.begin();
        drive(&mut panel, task).await;
        assert_eq!(panel.stage(), &Stage::Sign);
        assert!(!root.exists());
        let file = sign_to_file(&panel, &scan.wallet.signers, &temp.0, "signed.txt");
        let task = panel.import_from(vec![file]);
        let mut recorded = None;
        for event in events(task).await {
            match event {
                SplitEvent::Imported(..) => {
                    for event in events(panel.apply(event)).await {
                        recorded = Some(event);
                    }
                }
                other => panic!("{:?}", other),
            }
        }
        let digest = panel.construction().unwrap().source().digest();
        let expected = step1::journal_directory(&root, digest);
        let recorded = recorded.expect("a recording result");
        match &recorded {
            SplitEvent::Recorded(_, Err((_, found))) => {
                assert_eq!(found, &written.then(|| (digest, expected.clone())))
            }
            other => panic!("{:?}", other),
        }
        assert_eq!(panel.journal_directory(), None);
        drop(panel.apply(recorded));
        assert!(matches!(panel.stage(), Stage::Refused(refusal) if refusal.retry));
        assert_eq!(panel.journal_directory(), written.then_some(&expected));
    }
}

/// build → export → import → finalize → record → review → submit, for a
/// single-key wallet whose signed txid differs (pkh) and a 2-of-3 multisig
/// signed by two separate files.
#[tokio::test(flavor = "multi_thread")]
async fn split_panel_flow_builds_exports_imports_records_reviews_and_submits() {
    for shape in [Shape::Pkh, Shape::WshSortedMulti] {
        let scan = Scan::new(shape);
        let connect = FakeConnect::new(&scan.coins);
        let temp = Temp::new();
        let mut panel = recorded(&scan, &connect, &temp).await;

        // Preconditions: the destination is the proven fresh index, read
        // afresh on both chains.
        let prepared = panel.prepared().unwrap();
        let intent = scan.intent();
        assert_eq!(
            crate::services::foreign_split_inventory::FreshIndex::Proven(prepared.destination),
            intent.inventory.fresh_receive()
        );
        let reads = connect.calls.address_reads.lock().unwrap().clone();
        assert_eq!(
            reads,
            vec![
                (ChainId::Bitcoin, prepared.address.clone()),
                (ChainId::BitcoinBlake2b, prepared.address.clone())
            ]
        );
        // Built against the anchor's fork height and marker, with locktime =
        // the Bitcoin tip height.
        let construction = panel.construction().unwrap();
        assert_eq!(construction.fork_height(), fixture::FORK);
        assert_eq!(
            construction.fork_marker(),
            fixture::block_hash(fixture::FORK)
        );
        assert_eq!(
            construction.psbt().unsigned_tx.lock_time,
            LockTime::from_height(fixture::BITCOIN_TIP_HEIGHT).unwrap()
        );
        assert_eq!(construction.destination(), prepared.destination);

        // The tracked txid is the signed one.
        let signed = panel.signed().unwrap().clone();
        assert_eq!(panel.tracked_txid(), Some(signed.compute_txid()));
        assert_eq!(
            signed.compute_txid() == construction.txid(),
            shape == Shape::WshSortedMulti
        );
        // The journal is in the Split directory under the source digest.
        let digest = construction.source().digest();
        assert_eq!(
            panel.journal_directory(),
            Some(&temp.root().join(digest.to_string()))
        );
        assert_eq!(*connect.calls.opened.lock().unwrap(), vec![false]);

        // Review only on request, then submit exactly it.
        assert_eq!(connect.calls.reviews.load(Ordering::SeqCst), 0);
        let task = panel.update(SplitMessage::Review);
        drive(&mut panel, task).await;
        assert_eq!(panel.stage(), &Stage::Review);
        assert_eq!(panel.review().unwrap().txid, signed.compute_txid());
        let task = panel.update(SplitMessage::Confirm);
        drive(&mut panel, task).await;
        assert_eq!(connect.calls.submits.load(Ordering::SeqCst), 1);
        assert_eq!(panel.stage(), &Stage::Tracking);
        assert!(matches!(panel.outcome(), Some(Outcome::Uncertain { .. })));
        // Tracking never re-submits.
        let task = panel.update(SplitMessage::Confirm);
        drive(&mut panel, task).await;
        let task = panel.update(SplitMessage::Review);
        drive(&mut panel, task).await;
        assert_eq!(connect.calls.submits.load(Ordering::SeqCst), 1);
        assert_eq!(connect.calls.reviews.load(Ordering::SeqCst), 1);
        let task = panel.update(SplitMessage::Reconcile);
        drive(&mut panel, task).await;
        assert_eq!(connect.calls.reconciles.load(Ordering::SeqCst), 1);
        assert_eq!(panel.stage(), &Stage::Tracking);
    }
}

/// A refused submission (preflight) keeps the signed step 1, shows the
/// reason and can save it.
#[tokio::test(flavor = "multi_thread")]
async fn split_preflight_refusal_keeps_the_signed_transaction_for_export() {
    let scan = Scan::new(Shape::Wpkh);
    let connect = FakeConnect::new(&scan.coins);
    *connect.submit.lock().unwrap() = SubmitPlan::Refuse("min relay fee not met".into());
    let temp = Temp::new();
    let mut panel = recorded(&scan, &connect, &temp).await;
    let signed = panel.signed().unwrap().clone();
    let task = panel.update(SplitMessage::Review);
    drive(&mut panel, task).await;
    let task = panel.update(SplitMessage::Confirm);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Ready);
    assert!(panel.notice().unwrap().contains("min relay fee not met"));
    assert_eq!(panel.signed(), Some(&signed));
    let path = temp.0.join("signed.txt");
    let task = panel.export_signed_to(path.clone());
    drive(&mut panel, task).await;
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        serialize_hex(&signed)
    );

    // The destination is proven unused again at every review: a used one
    // refuses before the coordinator is asked, and the signed step 1 stays.
    let reviews = connect.calls.reviews.load(Ordering::SeqCst);
    connect
        .used
        .lock()
        .unwrap()
        .insert(ChainId::Bitcoin, Ok(true));
    let task = panel.update(SplitMessage::Review);
    drive(&mut panel, task).await;
    assert_eq!(connect.calls.reviews.load(Ordering::SeqCst), reviews);
    assert_eq!(panel.stage(), &Stage::Ready);
    assert_eq!(panel.notice(), Some(step1::DESTINATION_USED));
    assert_eq!(panel.signed(), Some(&signed));
    let destination = step1::construction_destination(panel.construction().unwrap()).unwrap();
    assert_eq!(
        connect.calls.address_reads.lock().unwrap().last(),
        Some(&(ChainId::Bitcoin, destination))
    );
}

/// Every precondition refuses before anything is built or recorded.
#[tokio::test(flavor = "multi_thread")]
async fn split_preconditions_refuse_before_building() {
    let scan = Scan::new(Shape::ShWpkh);
    type Case<'a> = (
        &'a str,
        Box<dyn Fn(&FakeConnect)>,
        Option<SplitIntent>,
        &'a str,
    );
    let cases: Vec<Case> = vec![
        (
            "rdts inside the 36 h margin",
            Box::new(|c: &FakeConnect| {
                let mut w = window();
                w.expires_at = w.median_time_past + 35 * 3600;
                w.rdts = Err(Assessment::ExpiryMargin);
                *c.window.lock().unwrap() = Ok(w);
            }),
            None,
            "expires too soon",
        ),
        (
            "stale anchor",
            Box::new(|c: &FakeConnect| {
                let mut w = window();
                w.fork_height += 1;
                *c.window.lock().unwrap() = Ok(w);
            }),
            None,
            step1::STALE_ANCHOR,
        ),
        (
            "destination used on BTCB2",
            Box::new(|c: &FakeConnect| {
                c.used
                    .lock()
                    .unwrap()
                    .insert(ChainId::BitcoinBlake2b, Ok(true));
            }),
            None,
            step1::DESTINATION_USED,
        ),
        (
            "destination freshness unreadable",
            Box::new(|c: &FakeConnect| {
                c.used
                    .lock()
                    .unwrap()
                    .insert(ChainId::Bitcoin, Err(FailureKind::Http(500)));
            }),
            None,
            "couldn't prove the fresh address unused",
        ),
        (
            "unproven fresh index (P7/D8)",
            Box::new(|_: &FakeConnect| {}),
            Some(scan.intent_with(|report| report.with_coverage(fixture::walk(4, Some(3))))),
            step1::WATCH_ONLY_DEFERRED,
        ),
    ];
    for (name, edit, intent, copy) in cases {
        let connect = FakeConnect::new(&scan.coins);
        edit(&connect);
        let temp = Temp::new();
        let mut panel = SplitPanel::start(
            TARGET.into(),
            temp.root(),
            intent.unwrap_or_else(|| scan.intent()),
        );
        panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
        let task = panel.begin();
        drive(&mut panel, task).await;
        match panel.stage() {
            Stage::Refused(refusal) => {
                assert!(refusal.reason.contains(copy), "{}: {:?}", name, refusal)
            }
            other => panic!("{}: {:?}", name, other),
        }
        assert!(panel.construction().is_none(), "{}", name);
        assert!(connect.calls.opened.lock().unwrap().is_empty(), "{}", name);
        assert!(!temp.root().exists(), "{}", name);
    }
}

/// Sign-out or cancel revokes the coordinator synchronously and drops any
/// in-flight result; the recorded split stays on disk.
#[tokio::test(flavor = "multi_thread")]
async fn split_session_end_revokes_and_drops_in_flight_results() {
    let scan = Scan::new(Shape::Wpkh);
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let mut panel = recorded(&scan, &connect, &temp).await;
    let review = panel.update(SplitMessage::Review);
    panel.revoke();
    assert!(connect.calls.revoked.load(Ordering::SeqCst));
    assert!(!panel.is_bound());
    assert_eq!(panel.stage(), &Stage::NeedsSession);
    // The review started before the revocation lands and is dropped.
    drive(&mut panel, review).await;
    assert!(panel.review().is_none());
    assert!(!panel.is_bound());
    assert_eq!(panel.stage(), &Stage::NeedsSession);
    // Clearing the session keeps it that way; the journal is untouched.
    panel.set_connect(None);
    assert_eq!(step1::discover(&temp.root()).len(), 1);
    let task = panel.update(SplitMessage::Confirm);
    drive(&mut panel, task).await;
    assert_eq!(connect.calls.submits.load(Ordering::SeqCst), 0);

    // Cancel (Close) on a bound panel revokes and hides; a new session does
    // not wake a hidden panel.
    let connect = FakeConnect::new(&scan.coins);
    drop(panel);
    let mut panel = resumed(&connect, &temp).await;
    assert!(panel.is_bound());
    let task = panel.update(SplitMessage::Close);
    drive(&mut panel, task).await;
    assert!(connect.calls.revoked.load(Ordering::SeqCst));
    assert!(panel.is_hidden() && !panel.is_bound());
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert!(!panel.is_bound());
    assert_eq!(step1::discover(&temp.root()).len(), 1);
}

fn rewrite_journal(directory: &Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let path = directory.join("intent.json");
    let mut value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    edit(&mut value);
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
}

async fn resumed(connect: &Arc<FakeConnect>, temp: &Temp) -> SplitPanel {
    let found = step1::discover(&temp.root());
    assert_eq!(found.len(), 1);
    let (digest, directory) = found.into_iter().next().unwrap();
    let mut panel = SplitPanel::resume(TARGET.into(), temp.root(), digest, directory);
    panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
    let task = panel.begin();
    drive(&mut panel, task).await;
    panel
}

/// Restart at Intent and at BroadcastUncertain: the recorded step 1 is
/// rebuilt from freshly authenticated coins and resumed with exactly the
/// recorded signed bytes. Nothing is reviewed, re-signed or submitted
/// automatically; a recorded submission is only reconciled.
#[tokio::test(flavor = "multi_thread")]
async fn split_restart_rebuilds_the_exact_recorded_bytes_without_retrying() {
    for shape in [Shape::Pkh, Shape::WshMulti] {
        let scan = Scan::new(shape);
        let connect = FakeConnect::new(&scan.coins);
        let temp = Temp::new();
        let first = recorded(&scan, &connect, &temp).await;
        let signed = first.signed().unwrap().clone();
        let unsigned = first.construction().unwrap().psbt().clone();
        drop(first);

        // At Intent.
        let panel = resumed(&connect, &temp).await;
        assert_eq!(panel.stage(), &Stage::Ready, "{:?}", panel.stage());
        assert_eq!(panel.signed(), Some(&signed));
        assert_eq!(panel.construction().unwrap().psbt(), &unsigned);
        assert_eq!(panel.phase(), Some(Phase::Intent));
        assert_eq!(*connect.calls.opened.lock().unwrap(), vec![false, true]);
        assert_eq!(connect.calls.reviews.load(Ordering::SeqCst), 0);
        assert_eq!(connect.calls.submits.load(Ordering::SeqCst), 0);
        // Abandon is offered, but confirmed only after a chain check.
        assert!(panel.can_check_abandon() && !panel.can_confirm_abandon());
        drop(panel);

        // At BroadcastUncertain.
        let directory = step1::discover(&temp.root()).remove(0).1;
        rewrite_journal(&directory, |intent| {
            intent["phase"] = "BroadcastUncertain".into();
            intent["signed_txid"] = signed.compute_txid().to_string().into();
            intent["bitcoin_attempts"] =
                serde_json::json!([{ "wtxid": signed.compute_wtxid().to_string() }]);
        });
        let mut panel = resumed(&connect, &temp).await;
        assert_eq!(panel.stage(), &Stage::Tracking, "{:?}", panel.stage());
        assert_eq!(panel.signed(), Some(&signed));
        assert_eq!(panel.phase(), Some(Phase::BroadcastUncertain));
        assert!(!panel.can_check_abandon());
        for message in [
            SplitMessage::Review,
            SplitMessage::Confirm,
            SplitMessage::CheckAbandon,
        ] {
            let task = panel.update(message);
            drive(&mut panel, task).await;
        }
        assert_eq!(connect.calls.reviews.load(Ordering::SeqCst), 0);
        assert_eq!(connect.calls.submits.load(Ordering::SeqCst), 0);
        assert_eq!(panel.stage(), &Stage::Tracking);
        assert_eq!(step1::discover(&temp.root()).len(), 1);
    }
}

/// Restart refuses, keeping the journal, when the recorded step 1 can't be
/// rebuilt: a coin spent on BTCB2, a moved anchor, or an indexer read error
/// (worded as a Connect limit, never as "spent").
#[tokio::test(flavor = "multi_thread")]
async fn split_restart_refuses_without_fresh_evidence() {
    let scan = Scan::new(Shape::Wpkh);
    let temp = Temp::new();
    {
        let connect = FakeConnect::new(&scan.coins);
        recorded(&scan, &connect, &temp).await;
    }
    let connect = FakeConnect::new(&scan.coins);
    connect
        .chains
        .spend_on(ChainId::BitcoinBlake2b, scan.coins[0].outpoint);
    let panel = resumed(&connect, &temp).await;
    assert!(
        matches!(panel.stage(), Stage::Refused(r) if r.reason.contains("no longer unspent on Bitcoin Blake2b"))
    );
    assert!(!panel.is_bound());

    let connect = FakeConnect::new(&scan.coins);
    let mut moved = window();
    moved.fork_height -= 1;
    *connect.window.lock().unwrap() = Ok(moved);
    let panel = resumed(&connect, &temp).await;
    assert!(matches!(panel.stage(), Stage::Refused(r) if r.reason == step1::STALE_ANCHOR));

    let refusal = step1::evidence_refusal(crate::services::split_evidence::EvidenceError {
        outpoint: None,
        failure: crate::services::split_evidence::EvidenceFailure::Read(
            ChainId::BitcoinBlake2b,
            FailureKind::Http(400),
        ),
    });
    assert!(refusal.retry);
    assert!(refusal.reason.contains("not a sign that a coin was spent"));
    assert!(refusal.reason.contains("500"));
    assert_eq!(step1::discover(&temp.root()).len(), 1);
}

/// Abandon needs a passing chain check first; a spent coin (for example an
/// out-of-band broadcast) or a seen step 1 refuses it, and the journal stays.
#[tokio::test(flavor = "multi_thread")]
async fn split_abandon_only_after_a_chain_check() {
    let scan = Scan::new(Shape::Wpkh);
    let temp = Temp::new();
    let connect = FakeConnect::new(&scan.coins);
    let mut panel = recorded(&scan, &connect, &temp).await;
    assert!(panel.can_check_abandon() && !panel.can_confirm_abandon());
    panel.revoke();
    drop(panel);

    let mut panel = resumed(&connect, &temp).await;
    // Confirming without a check is ignored.
    let task = panel.update(SplitMessage::ConfirmAbandon);
    drive(&mut panel, task).await;
    assert_eq!(step1::discover(&temp.root()).len(), 1);

    // A coin spent on Bitcoin refuses.
    connect
        .chains
        .spend_on(ChainId::Bitcoin, scan.coins[1].outpoint);
    let task = panel.update(SplitMessage::CheckAbandon);
    drive(&mut panel, task).await;
    assert!(!panel.can_confirm_abandon());
    assert!(panel.notice().unwrap().contains("spent on Bitcoin"));
    assert_eq!(panel.stage(), &Stage::Ready);

    // The step 1 itself seen on Bitcoin refuses.
    let connect = FakeConnect::new(&scan.coins);
    let tracked = panel.tracked_txid().unwrap();
    connect.chains.status.lock().unwrap().insert(
        (ChainId::Bitcoin, tracked),
        TransactionObservation::Unconfirmed { txid: tracked },
    );
    drop(panel);
    let mut panel = resumed(&connect, &temp).await;
    let task = panel.update(SplitMessage::CheckAbandon);
    drive(&mut panel, task).await;
    assert!(!panel.can_confirm_abandon());
    assert!(panel.notice().unwrap().contains("on Bitcoin"));

    // A clean chain: check, then abandon deletes the journal.
    let connect = FakeConnect::new(&scan.coins);
    drop(panel);
    let mut panel = resumed(&connect, &temp).await;
    let task = panel.update(SplitMessage::CheckAbandon);
    drive(&mut panel, task).await;
    assert!(panel.can_confirm_abandon(), "{:?}", panel.notice());
    let task = panel.update(SplitMessage::ConfirmAbandon);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Abandoned, "{:?}", panel.stage());
    assert!(connect.calls.revoked.load(Ordering::SeqCst));
    assert!(step1::discover(&temp.root()).is_empty());
}

/// Flip one byte of the recorded step 1's first witness signature (#625
/// review F2's probe). The journal still validates, since it only checks that
/// a signature is present, but the signed bytes no longer verify. The txid
/// is unchanged: a witness is not part of it.
fn tamper_witness(directory: &Path) -> Transaction {
    let mut tampered = None;
    rewrite_journal(directory, |intent| {
        let mut signed: Transaction =
            serde_json::from_value(intent["bitcoin_transaction"].clone()).unwrap();
        let mut items: Vec<Vec<u8>> = signed.input[0].witness.iter().map(<[u8]>::to_vec).collect();
        items[0][10] ^= 1;
        signed.input[0].witness = coincube_core::miniscript::bitcoin::Witness::from_slice(&items);
        intent["bitcoin_transaction"] = serde_json::to_value(&signed).unwrap();
        tampered = Some(signed);
    });
    tampered.unwrap()
}

/// #625 F2, step 1: an unsubmitted journal whose recorded step 1 doesn't
/// verify is offered for abandonment only. It holds no construction, signed
/// bytes or coordinator, so nothing can be reviewed, exported or sent, and
/// the abandon still needs a passing chain check, repeated at confirmation.
/// The journal is kept when the step 1 is seen, a coin is spent, evidence
/// fails or is stale, the session changes, or a submission is recorded.
#[tokio::test(flavor = "multi_thread")]
async fn split_unrebuildable_journal_is_abandoned_only_after_a_chain_check() {
    let scan = Scan::new(Shape::Wpkh);
    let temp = Temp::new();
    let (tracked, claimed) = {
        let connect = FakeConnect::new(&scan.coins);
        let panel = recorded(&scan, &connect, &temp).await;
        (
            panel.tracked_txid().unwrap(),
            step1::claimed_addresses(panel.construction().unwrap()),
        )
    };
    let directory = step1::discover(&temp.root()).remove(0).1;
    let tampered = tamper_witness(&directory);
    assert_eq!(tampered.compute_txid(), tracked);
    let kept = |temp: &Temp| assert_eq!(step1::discover(&temp.root()).len(), 1);

    let connect = FakeConnect::new(&scan.coins);
    let mut panel = resumed(&connect, &temp).await;
    assert!(
        matches!(panel.stage(), Stage::Refused(r) if r.reason.contains("does not verify") && !r.retry),
        "{:?}",
        panel.stage()
    );
    assert_eq!(
        panel.abandon_only(),
        Some(&step1::AbandonOnly {
            tracked,
            claimed: claimed.clone(),
        })
    );
    assert_eq!(panel.notice(), Some(step1::UNREBUILDABLE));
    assert_eq!(panel.phase(), Some(Phase::Intent));
    assert!(panel.can_check_abandon() && !panel.can_confirm_abandon());
    // Nothing to review, export or send: no driver, construction or bytes.
    assert!(!panel.is_bound());
    assert!(panel.construction().is_none() && panel.signed().is_none());
    for message in [
        SplitMessage::Review,
        SplitMessage::Confirm,
        SplitMessage::Reconcile,
        SplitMessage::ExportSigned,
        SplitMessage::EnterStep2,
        SplitMessage::Retry,
        // Confirming without a check is ignored.
        SplitMessage::ConfirmAbandon,
    ] {
        let task = panel.update(message);
        drive(&mut panel, task).await;
    }
    assert!(matches!(panel.stage(), Stage::Refused(_)));
    assert_eq!(*connect.calls.opened.lock().unwrap(), Vec::<bool>::new());
    assert_eq!(connect.calls.reviews.load(Ordering::SeqCst), 0);
    assert_eq!(connect.calls.submits.load(Ordering::SeqCst), 0);
    kept(&temp);

    // The check refuses, and the journal stays, when: a coin is spent on
    // Bitcoin; the step 1 is seen there; a read fails; a read is stale.
    let check = |panel: &mut SplitPanel| panel.update(SplitMessage::CheckAbandon);
    connect
        .chains
        .spend_on(ChainId::Bitcoin, scan.coins[0].outpoint);
    let task = check(&mut panel);
    drive(&mut panel, task).await;
    assert!(!panel.can_confirm_abandon());
    assert!(panel.notice().unwrap().contains("spent on Bitcoin"));
    assert!(matches!(panel.stage(), Stage::Refused(_)));
    let connect = FakeConnect::new(&scan.coins);
    connect.chains.status.lock().unwrap().insert(
        (ChainId::Bitcoin, tracked),
        TransactionObservation::Unconfirmed { txid: tracked },
    );
    let mut panel = resumed(&connect, &temp).await;
    let task = check(&mut panel);
    drive(&mut panel, task).await;
    assert!(!panel.can_confirm_abandon());
    assert!(panel.notice().unwrap().contains("on Bitcoin"));
    for fault in [Fault::Error, Fault::Stale] {
        let connect = FakeConnect::new(&scan.coins);
        *connect.chains.utxo_fault.lock().unwrap() = Some(fault);
        let mut panel = resumed(&connect, &temp).await;
        let task = check(&mut panel);
        drive(&mut panel, task).await;
        assert!(!panel.can_confirm_abandon(), "{:?}", fault);
        assert!(
            panel
                .notice()
                .unwrap()
                .contains("not a sign that a coin was spent"),
            "{:?}",
            panel.notice()
        );
    }
    kept(&temp);

    // A passing check, then a coin spent before confirmation: the check is
    // repeated at confirmation, and refuses.
    let connect = FakeConnect::new(&scan.coins);
    let mut panel = resumed(&connect, &temp).await;
    let task = check(&mut panel);
    drive(&mut panel, task).await;
    assert!(panel.can_confirm_abandon(), "{:?}", panel.notice());
    connect
        .chains
        .spend_on(ChainId::Bitcoin, scan.coins[0].outpoint);
    let task = panel.update(SplitMessage::ConfirmAbandon);
    drive(&mut panel, task).await;
    assert!(
        matches!(panel.stage(), Stage::Refused(r) if r.reason.contains("spent on Bitcoin")),
        "{:?}",
        panel.stage()
    );
    kept(&temp);

    // A passing check, then the session changes: no confirmation without a
    // new check under the next session, which reads the journal again.
    let connect = FakeConnect::new(&scan.coins);
    let mut panel = resumed(&connect, &temp).await;
    let task = check(&mut panel);
    drive(&mut panel, task).await;
    assert!(panel.can_confirm_abandon());
    panel.revoke();
    assert!(!panel.can_check_abandon() && panel.abandon_only().is_none());
    let task = panel.update(SplitMessage::ConfirmAbandon);
    drive(&mut panel, task).await;
    kept(&temp);
    // The session ends after the abandon was confirmed, before its task
    // deletes the journal (#644 r4176212750): it is kept.
    let connect = FakeConnect::new(&scan.coins);
    let mut confirmed = resumed(&connect, &temp).await;
    let task = check(&mut confirmed);
    drive(&mut confirmed, task).await;
    assert!(confirmed.can_confirm_abandon());
    let task = confirmed.update(SplitMessage::ConfirmAbandon);
    confirmed.revoke();
    drive(&mut confirmed, task).await;
    kept(&temp);
    assert_eq!(confirmed.stage(), &Stage::NeedsSession);
    drop(confirmed);
    // The next session can't read Bitcoin Blake2b: the rebuild may pass on
    // retry, so nothing is offered for abandonment from the last session.
    let unread = FakeConnect::new(&scan.coins);
    *unread.window.lock().unwrap() = Err("unreachable".into());
    panel.set_connect(Some(unread.clone() as Arc<dyn SplitConnect>));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert!(matches!(panel.stage(), Stage::Refused(r) if r.retry));
    assert!(!panel.can_check_abandon() && panel.abandon_only().is_none());

    // A passing check, then a submission recorded elsewhere before
    // confirmation: the journal refuses to be abandoned.
    let connect = FakeConnect::new(&scan.coins);
    let mut panel = resumed(&connect, &temp).await;
    let task = check(&mut panel);
    drive(&mut panel, task).await;
    assert!(panel.can_confirm_abandon());
    let submitted = |directory: &Path| {
        rewrite_journal(directory, |intent| {
            intent["phase"] = "BroadcastUncertain".into();
            intent["signed_txid"] = tracked.to_string().into();
            intent["bitcoin_attempts"] =
                serde_json::json!([{ "wtxid": tampered.compute_wtxid().to_string() }]);
        })
    };
    let pristine = std::fs::read(directory.join("intent.json")).unwrap();
    submitted(&directory);
    let task = panel.update(SplitMessage::ConfirmAbandon);
    drive(&mut panel, task).await;
    assert!(
        matches!(panel.stage(), Stage::Refused(r) if r.reason.contains("could not be abandoned")),
        "{:?}",
        panel.stage()
    );
    kept(&temp);
    // A recorded submission is never offered for abandonment here.
    drop(panel);
    let mut panel = resumed(&connect, &temp).await;
    assert_eq!(panel.notice(), Some(step1::SUBMISSION_RECORDED));
    assert!(!panel.can_check_abandon() && panel.abandon_only().is_none());
    let task = panel.update(SplitMessage::CheckAbandon);
    drive(&mut panel, task).await;
    kept(&temp);
    std::fs::write(directory.join("intent.json"), &pristine).unwrap();

    // Clean chains: check, then confirm deletes the journal.
    drop(panel);
    let connect = FakeConnect::new(&scan.coins);
    let mut panel = resumed(&connect, &temp).await;
    let task = check(&mut panel);
    drive(&mut panel, task).await;
    assert!(panel.can_confirm_abandon(), "{:?}", panel.notice());
    let task = panel.update(SplitMessage::ConfirmAbandon);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Abandoned, "{:?}", panel.stage());
    assert!(panel.abandon_only().is_none());
    assert!(step1::discover(&temp.root()).is_empty());
}

/// #625 F2: when the recorded inputs can't be established on Bitcoin, the
/// journal is kept with an explicit refusal; a read failure may pass on
/// retry, an input whose previous transaction doesn't match its txid not.
#[tokio::test(flavor = "multi_thread")]
async fn split_unrebuildable_journal_with_unestablished_inputs_is_kept() {
    let scan = Scan::new(Shape::Wpkh);
    let temp = Temp::new();
    {
        let connect = FakeConnect::new(&scan.coins);
        recorded(&scan, &connect, &temp).await;
    }
    let directory = step1::discover(&temp.root()).remove(0).1;
    tamper_witness(&directory);
    let first = scan.coins[0].outpoint.txid;

    // A previous transaction that isn't the one its txid names.
    let mut connect = FakeConnect::new(&scan.coins);
    let other = scan.coins[1].previous.clone();
    assert_ne!(other.compute_txid(), first);
    Arc::get_mut(&mut connect)
        .unwrap()
        .chains
        .previous
        .insert(first, other);
    let panel = resumed(&connect, &temp).await;
    assert!(matches!(panel.stage(), Stage::Refused(r) if !r.retry));
    assert!(!panel.can_check_abandon() && panel.abandon_only().is_none());
    assert_eq!(step1::discover(&temp.root()).len(), 1);

    // A previous transaction that can't be read, with the rebuild refused
    // first by a moved anchor: kept, and the refusal may be retried.
    let mut connect = FakeConnect::new(&scan.coins);
    Arc::get_mut(&mut connect)
        .unwrap()
        .chains
        .previous
        .remove(&first);
    let mut moved = window();
    moved.fork_height -= 1;
    *connect.window.lock().unwrap() = Ok(moved);
    let panel = resumed(&connect, &temp).await;
    assert!(
        matches!(panel.stage(), Stage::Refused(r) if r.reason == step1::STALE_ANCHOR && r.retry),
        "{:?}",
        panel.stage()
    );
    assert!(panel
        .notice()
        .unwrap()
        .contains("not a sign that a coin was spent"));
    assert!(!panel.can_check_abandon() && panel.abandon_only().is_none());
    assert_eq!(step1::discover(&temp.root()).len(), 1);
}

/// The datadir holds public data only: no seed, xpriv or private key, and the
/// journal is owner-only (file 0600, directory 0700).
#[tokio::test(flavor = "multi_thread")]
async fn split_journal_is_private_and_holds_no_secrets() {
    let scan = Scan::new(Shape::WshSortedMulti);
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let panel = recorded(&scan, &connect, &temp).await;
    let directory = panel.journal_directory().unwrap().clone();
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&directory.join("intent.json")), 0o600);
    assert_eq!(mode(&directory), 0o700);
    assert_eq!(mode(&temp.root()), 0o700);

    let secp = Secp256k1::new();
    let mut secrets: Vec<String> = Vec::new();
    for signer in &scan.wallet.signers {
        secrets.push(signer.to_string());
        secrets.push(hex::encode(signer.private_key.secret_bytes()));
        secrets.push(signer.private_key.display_secret().to_string());
        let _ = &secp;
    }
    for seed in [1u8, 2, 3] {
        secrets.push(hex::encode([seed; 32]));
    }
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&temp.root(), &mut files);
    assert!(files.iter().any(|f| f.ends_with("intent.json")));
    for file in files {
        let text = String::from_utf8_lossy(&std::fs::read(&file).unwrap()).to_string();
        for marker in ["xprv", "tprv"] {
            assert!(!text.contains(marker), "{}", file.display());
        }
        for secret in &secrets {
            assert!(!text.contains(secret.as_str()), "{}", file.display());
        }
    }
}

/// Journal discovery: only a real digest-named directory holding a regular
/// intent.json; nothing else (including a Claim pairing's `claim/` layout or
/// a symlink) is a Split journal.
#[test]
fn split_journal_discovery_finds_only_split_journals() {
    let temp = Temp::new();
    let root = temp.root();
    assert!(step1::discover(&root).is_empty());
    std::fs::create_dir_all(&root).unwrap();
    let digest = sha256::Hash::hash(b"source");
    let real = step1::journal_directory(&root, digest);
    std::fs::create_dir(&real).unwrap();
    assert!(step1::discover(&root).is_empty());
    std::fs::write(real.join("intent.json"), b"{}").unwrap();
    // Not a digest, a symlinked digest directory, an uppercase digest name.
    let claim = root.join("claim");
    std::fs::create_dir(&claim).unwrap();
    std::fs::write(claim.join("intent.json"), b"{}").unwrap();
    let other = sha256::Hash::hash(b"other");
    std::os::unix::fs::symlink(&real, step1::journal_directory(&root, other)).unwrap();
    let upper = root.join(sha256::Hash::hash(b"upper").to_string().to_uppercase());
    std::fs::create_dir(&upper).unwrap();
    std::fs::write(upper.join("intent.json"), b"{}").unwrap();
    assert_eq!(step1::discover(&root), vec![(digest, real.clone())]);
    // #625 F2: a tombstone that is not a regular file doesn't close it; a
    // regular one does, and discovery skips the journal.
    std::fs::create_dir(real.join(step1::CLOSED)).unwrap();
    assert!(!step1::is_closed(&real));
    std::fs::remove_dir(real.join(step1::CLOSED)).unwrap();
    std::os::unix::fs::symlink(real.join("intent.json"), real.join(step1::CLOSED)).unwrap();
    assert!(!step1::is_closed(&real));
    assert_eq!(step1::discover(&root), vec![(digest, real.clone())]);
    std::fs::remove_file(real.join(step1::CLOSED)).unwrap();
    std::fs::write(real.join(step1::CLOSED), b"{}").unwrap();
    assert!(step1::is_closed(&real));
    assert!(step1::discover(&root).is_empty());
}

/// D1: nothing in the GUI starts a split. `SplitPanel::start` is reached only
/// from this module's tests; production constructs the panel only through
/// `SplitPanel::resume` in `app/mod.rs`'s journal discovery; the panel's
/// intents have no start; and the Home scan's review overlay still offers
/// only its close action.
#[test]
fn split_panel_has_no_gui_entry_point() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    fn walk(dir: &Path, files: &mut Vec<(String, String)>, root: &Path) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, files, root);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push((
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    std::fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    walk(&root, &mut files, &root);
    for (file, text) in &files {
        let starts = text.matches("SplitPanel::start").count();
        if !file.starts_with("app/state/vault/split/") {
            assert_eq!(starts, 0, "{} starts a split", file);
        }
        let resumes =
            text.matches("SplitPanel::resume").count() + text.matches("SplitPanel::{").count();
        if !file.starts_with("app/state/vault/split/") {
            match file.as_str() {
                "app/mod.rs" => assert_eq!(resumes, 1, "{}", file),
                _ => assert_eq!(resumes, 0, "{} constructs the Split panel", file),
            }
        }
    }
    let app = &files.iter().find(|(f, _)| f == "app/mod.rs").unwrap().1;
    // The one construction is the journal discovery's resume.
    let discovery = &app[app.find("fn discover_split_panel(").unwrap()..];
    let discovery = &discovery[..discovery.find("\n}\n").unwrap()];
    assert!(discovery.contains("step1::discover(&root)"));
    assert!(discovery.contains("SplitPanel::resume("));
    assert!(!app.contains("SplitPanel::start"));
    // #625 F3a: that discovery is called from production only in the App's
    // discovery task, off the UI thread; its result is installed by
    // `Message::SplitDiscovered`.
    let production = &app[..app.find("\n#[cfg(test)]\n").unwrap()];
    let calls: Vec<_> = production
        .match_indices("discover_split_panel(")
        .filter(|(at, _)| !production[..*at].ends_with("fn "))
        .collect();
    assert_eq!(calls.len(), 1, "{:?}", calls);
    let task = &production[production.find("fn split_discovery_task(").unwrap()..];
    let task = &task[..task.find("\n    }\n").unwrap()];
    let blocking = task.find("spawn_blocking(move ||").unwrap();
    assert!(task[blocking..].contains("discover_split_panel(&datadir, &settings, &wallet)"));
    assert_eq!(
        production.matches("self.split_panel = Some(").count(),
        1,
        "the panel is installed only from the discovery result"
    );
    // The review overlay's only action is its close.
    let overlay = &app[app.find("fn split_review_overlay<").unwrap()..];
    let overlay = &overlay[..overlay.find("\n}\n").unwrap()];
    let presses: Vec<_> = overlay.match_indices(".on_press(").collect();
    assert_eq!(presses.len(), 1);
    assert!(overlay.contains(".on_press(view::Message::DismissSplitReview)"));
    // No panel intent starts a split.
    let state = &files
        .iter()
        .find(|(f, _)| f == "app/state/vault/split/mod.rs")
        .unwrap()
        .1;
    let intents = &state[state.find("pub enum SplitMessage {").unwrap()..];
    let intents = &intents[..intents.find("\n}\n").unwrap()];
    assert!(!intents.to_lowercase().contains("start"));
}

/// #625 F3: the App's construction, its Connect refresh and the panel's
/// result handling do no blocking work on the UI thread. Split journal
/// discovery (`step1::discover`: a directory read and `symlink_metadata`
/// per entry) and the
/// port builds (`Production*::new`, `reqwest` clients) appear in them only
/// through a `spawn_blocking` task.
#[test]
fn split_ui_paths_do_no_blocking_work() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let read = |path: &str| std::fs::read_to_string(src.join(path)).unwrap();
    // The body of `signature` in `text`, up to its closing line at `indent`.
    fn body<'a>(text: &'a str, signature: &str, indent: &str) -> &'a str {
        let start = text
            .find(signature)
            .unwrap_or_else(|| panic!("{} not found", signature));
        let end = text[start..].find(&format!("\n{}}}\n", indent)).unwrap();
        // From after the signature line: the body only.
        let body = &text[start..start + end];
        &body[body.find('\n').unwrap()..]
    }
    // Split's own discovery only: the Claim's `ForkHandoff::discover` in
    // `new_inner` is another panel's (not #625 F3).
    const BLOCKING: [&str; 9] = [
        "step1::discover(",
        "discover_split_panel(",
        "read_dir",
        "symlink_metadata",
        "std::fs::",
        "ProductionConnect::new(",
        "ProductionStep2::new(",
        "ProductionRecon::new(",
        "find_journal(",
    ];
    let app = read("app/mod.rs");
    let panel = read("app/state/vault/split/mod.rs");
    let panel2 = read("app/state/vault/split/panel2.rs");
    let ui = [
        ("new_inner", body(&app, "    fn new_inner(", "    ")),
        (
            "refresh_split_session",
            body(&app, "    fn refresh_split_session(", "    "),
        ),
        (
            "install_split_ports",
            body(&app, "    fn install_split_ports(", "    "),
        ),
        ("apply", body(&panel, "    pub fn apply(", "    ")),
        (
            "apply_step2",
            body(&panel2, "    pub(super) fn apply_step2(", "    "),
        ),
    ];
    for (name, text) in ui {
        for token in BLOCKING {
            // Every occurrence must sit inside a `spawn_blocking(...)`
            // argument: after one, with its parentheses still open.
            for (at, _) in text.match_indices(token) {
                let inside = text[..at].rfind("spawn_blocking(").is_some_and(|open| {
                    let start = open + "spawn_blocking(".len();
                    // Closed once its depth reaches zero, for good.
                    let mut depth = 1i32;
                    text[start..at].chars().all(|c| {
                        match c {
                            '(' => depth += 1,
                            ')' => depth -= 1,
                            _ => {}
                        }
                        depth > 0
                    })
                });
                assert!(inside, "{} calls {} on the UI thread", name, token);
            }
        }
    }
    // The UI paths call the blocking helpers only through a task.
    let refresh = body(&app, "    fn refresh_split_session(", "    ");
    let blocking = refresh.find("spawn_blocking(move ||").unwrap();
    assert!(refresh[blocking..].contains("split_ports(Some(session), generation, daemon, site)"));
    assert!(!refresh[..blocking].contains("split_ports("));
    for (name, text) in [
        ("new_inner", body(&app, "    fn new_inner(", "    ")),
        ("apply", body(&panel, "    pub fn apply(", "    ")),
        (
            "install_split_ports",
            body(&app, "    fn install_split_ports(", "    "),
        ),
    ] {
        assert!(!text.contains("split_ports("), "{}", name);
        assert!(!text.contains("discover_split_panel("), "{}", name);
    }
    // The recorded refusal's journal lookup runs in the recording task.
    let record = body(&panel, "    fn maybe_record(", "    ");
    let lookup = record.find("find_journal(").unwrap();
    assert!(record[..lookup].ends_with("tokio::task::spawn_blocking(move || "));
}

/// The production Connect side: address freshness is a fresh, anonymous
/// read on Connect's allowlisted path of each chain, and `open` records and
/// resumes through the real Split coordinator (`Coordinator::create_split` /
/// `resume_split`) without any network call.
#[tokio::test(flavor = "multi_thread")]
async fn split_production_connect_reads_freshness_and_opens_the_real_coordinator() {
    use crate::services::split_test_connect::{serve_fresh, strict};
    use httpmock::MockServer;
    let server = MockServer::start();
    let refused = strict(&server);
    let mut client = CoincubeClient::new();
    client.base_url = format!("{}/", server.base_url());
    client.set_token("synthetic-test-token");
    let (_sender, generation) = tokio::sync::watch::channel(0u64);
    let connect = step1::ProductionConnect::new(
        crate::app::state::vault::claim::ConnectSession {
            client,
            account: "synthetic-account".into(),
        },
        generation,
    )
    .unwrap();

    let scan = Scan::new(Shape::Wpkh);
    let intent = scan.intent();
    let source =
        crate::services::split_source::split_source(&intent.external, intent.internal.as_ref())
            .unwrap();
    let crate::services::foreign_split_inventory::FreshIndex::Proven(index) =
        intent.inventory.fresh_receive()
    else {
        panic!("fixture has a fresh index");
    };
    let script = source
        .external()
        .at_derivation_index(index)
        .unwrap()
        .script_pubkey();
    let address = Address::from_script(&script, Network::Bitcoin)
        .unwrap()
        .to_string();
    let unused = r#"{"chain_stats":{"tx_count":0},"mempool_stats":{"tx_count":0}}"#;
    let used = r#"{"chain_stats":{"tx_count":0},"mempool_stats":{"tx_count":1}}"#;
    let path = format!("/address/{address}");
    let bitcoin = serve_fresh(&server, "bitcoin", &path, unused);
    let btcb2 = serve_fresh(&server, "bitcoin-blake2b", &path, used);
    assert_eq!(
        connect.address_used(ChainId::Bitcoin, &address).await,
        Ok(false)
    );
    assert_eq!(
        connect
            .address_used(ChainId::BitcoinBlake2b, &address)
            .await,
        Ok(true)
    );
    bitcoin.assert_hits(1);
    btcb2.assert_hits(1);
    refused.assert_hits(0);

    // Record, release, resume: the real coordinator over a real journal.
    let construction = step1::build(&step1::Prepared {
        source,
        coins: intent.inventory.splittable_coins(),
        destination: index,
        address,
        window: window(),
        feerate_vb: 3,
        bitcoin_tip_height: fixture::BITCOIN_TIP_HEIGHT,
    })
    .unwrap();
    let verify = || {
        let secp = Secp256k1::new();
        let mut psbt = construction.psbt().clone();
        for signer in &scan.wallet.signers {
            psbt.sign(signer, &secp).unwrap();
        }
        match step1::import(&construction, &[psbt]).unwrap() {
            step1::Imported::Complete(verified, _) => *verified,
            step1::Imported::Partial => panic!("fully signed"),
        }
    };
    let temp = Temp::new();
    let directory = step1::journal_directory(&temp.root(), construction.source().digest());
    let request = |resume| OpenRequest {
        directory: directory.clone(),
        target_cube: TARGET.into(),
        construction: construction.clone(),
        verified: verify(),
        fork_height: fixture::FORK,
        resume,
    };
    let driver = tokio::task::block_in_place(|| connect.open(request(false))).unwrap();
    assert_eq!(driver.phase(), Phase::Intent);
    let mode = std::fs::metadata(directory.join("intent.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    // A second record of the same source refuses while one exists.
    drop(driver);
    assert!(tokio::task::block_in_place(|| connect.open(request(false))).is_err());
    let driver = tokio::task::block_in_place(|| connect.open(request(true))).unwrap();
    assert_eq!(driver.phase(), Phase::Intent);
    refused.assert_hits(0);
}

/// A recorded and submitted step 1, resumed for tracking.
async fn tracking(scan: &Scan, connect: &Arc<FakeConnect>, temp: &Temp) -> SplitPanel {
    let first = recorded(scan, connect, temp).await;
    let signed = first.signed().unwrap().clone();
    drop(first);
    let directory = step1::discover(&temp.root()).remove(0).1;
    rewrite_journal(&directory, |intent| {
        intent["phase"] = "BroadcastUncertain".into();
        intent["signed_txid"] = signed.compute_txid().to_string().into();
        intent["bitcoin_attempts"] =
            serde_json::json!([{ "wtxid": signed.compute_wtxid().to_string() }]);
    });
    let panel = resumed(connect, temp).await;
    assert_eq!(panel.stage(), &Stage::Tracking, "{:?}", panel.stage());
    panel
}

async fn refresh(panel: &mut SplitPanel, connect: &Arc<FakeConnect>, status: Status) {
    *connect.calls.status.lock().unwrap() = Some(status);
    let task = panel.update(SplitMessage::Reconcile);
    drive(panel, task).await;
    assert_eq!(panel.stage(), &Stage::Tracking, "{:?}", panel.stage());
}

/// (#568 B2) Tracking shows step 1's Bitcoin depth as N of 6 at each
/// refresh; a reorg is shown as blocking step 2 and offers its review only
/// then. Nothing here reaches step 2.
#[tokio::test(flavor = "multi_thread")]
async fn split_tracking_counts_confirmations_to_six_and_flags_a_reorg() {
    let scan = Scan::new(Shape::Wpkh);
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let mut panel = tracking(&scan, &connect, &temp).await;
    assert_eq!(panel.confirmations(), None);

    // A reorg review is not offered without a reorg.
    let task = panel.update(SplitMessage::CheckReorg);
    drive(&mut panel, task).await;
    assert_eq!(connect.calls.recovers.load(Ordering::SeqCst), 0);

    refresh(
        &mut panel,
        &connect,
        Status::Observation(Assessment::WaitingForConfirmation),
    )
    .await;
    assert_eq!(panel.confirmations(), Some(0));
    for depth in 1..=5 {
        refresh(
            &mut panel,
            &connect,
            Status::Observation(Assessment::WaitingForDepth {
                confirmations: depth,
            }),
        )
        .await;
        assert_eq!(panel.confirmations(), Some(depth));
        assert!(!panel.reorged());
    }
    refresh(
        &mut panel,
        &connect,
        Status::Observation(Assessment::ObservationsEligibleForPreflight),
    )
    .await;
    assert_eq!(panel.confirmations(), Some(6));

    refresh(
        &mut panel,
        &connect,
        Status::Observation(Assessment::Reorged),
    )
    .await;
    assert!(panel.reorged());
    assert_eq!(panel.confirmations(), None);
    assert_eq!(connect.calls.reconciles.load(Ordering::SeqCst), 8);
    // Never a new review or submission of step 1 from tracking.
    assert_eq!(connect.calls.reviews.load(Ordering::SeqCst), 0);
    assert_eq!(connect.calls.submits.load(Ordering::SeqCst), 0);
}

/// Re-mined in another block: the new block is shown and must be
/// acknowledged; depth then counts from it at the next refresh.
#[tokio::test(flavor = "multi_thread")]
async fn split_reorg_remined_step1_needs_an_acknowledgement() {
    let scan = Scan::new(Shape::Pkh);
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let mut panel = tracking(&scan, &connect, &temp).await;
    refresh(
        &mut panel,
        &connect,
        Status::Observation(Assessment::Reorged),
    )
    .await;
    let previous = BlockRef {
        height: 100,
        hash: BlockHash::from_byte_array([6; 32]),
    };
    let confirmed = BlockRef {
        height: 101,
        hash: BlockHash::from_byte_array([9; 32]),
    };
    *connect.calls.recovery.lock().unwrap() = Some(Recovery::Reconfirmed {
        previous,
        confirmed,
    });
    // Sending again is not offered for a re-mined step 1.
    let task = panel.update(SplitMessage::ConfirmResend);
    drive(&mut panel, task).await;
    assert_eq!(connect.calls.resends.load(Ordering::SeqCst), 0);

    let task = panel.update(SplitMessage::CheckReorg);
    drive(&mut panel, task).await;
    assert_eq!(
        panel.stage(),
        &Stage::Reconfirm {
            previous,
            confirmed
        }
    );
    let task = panel.update(SplitMessage::ConfirmResend);
    drive(&mut panel, task).await;
    assert_eq!(connect.calls.resends.load(Ordering::SeqCst), 0);

    let task = panel.update(SplitMessage::AcknowledgeReconfirmation);
    drive(&mut panel, task).await;
    assert_eq!(connect.calls.acknowledges.load(Ordering::SeqCst), 1);
    assert_eq!(panel.stage(), &Stage::Tracking);
    assert_eq!(panel.status(), None);
    assert!(panel.notice().unwrap().contains("Check status again"));
    refresh(
        &mut panel,
        &connect,
        Status::Observation(Assessment::WaitingForDepth { confirmations: 2 }),
    )
    .await;
    assert_eq!(panel.confirmations(), Some(2));
}

/// Dropped from the chain: exactly the recorded step 1 is offered again
/// after its fresh review, sent only on confirmation, once.
#[tokio::test(flavor = "multi_thread")]
async fn split_reorg_dropped_step1_offers_the_exact_bytes_again() {
    let scan = Scan::new(Shape::Wpkh);
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let mut panel = tracking(&scan, &connect, &temp).await;
    let tracked = panel.tracked_txid().unwrap();
    refresh(
        &mut panel,
        &connect,
        Status::Observation(Assessment::Reorged),
    )
    .await;
    let review = ReviewView {
        txid: tracked,
        fee_sats: 1_000,
        vsize: 300,
        route: claim_coordinator::SubmissionRoute::Connect,
        bitcoin_tip: 106,
        fork_tip: u64::from(fixture::BTCB2_TIP_HEIGHT),
        rdts_left: Some(30 * 24 * 3600),
    };
    *connect.calls.recovery.lock().unwrap() = Some(Recovery::Resend(review.clone()));
    // Acknowledging is not offered for a dropped step 1.
    let task = panel.update(SplitMessage::AcknowledgeReconfirmation);
    drive(&mut panel, task).await;
    assert_eq!(connect.calls.acknowledges.load(Ordering::SeqCst), 0);

    let task = panel.update(SplitMessage::CheckReorg);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Resend);
    assert_eq!(panel.review(), Some(&review));
    let task = panel.update(SplitMessage::ConfirmResend);
    drive(&mut panel, task).await;
    assert_eq!(connect.calls.resends.load(Ordering::SeqCst), 1);
    assert_eq!(panel.stage(), &Stage::Tracking);
    assert!(matches!(panel.outcome(), Some(Outcome::Uncertain { txid, .. }) if txid == tracked));
    // A second confirm without a new review does nothing.
    let task = panel.update(SplitMessage::ConfirmResend);
    drive(&mut panel, task).await;
    assert_eq!(connect.calls.resends.load(Ordering::SeqCst), 1);
    assert_eq!(connect.calls.submits.load(Ordering::SeqCst), 0);
}

/// Dropped and nothing to send again: if a claimed coin was spent on
/// Bitcoin by another transaction, a new step 1 is needed (final); if not,
/// the reason is shown and tracking continues. A failed read is never
/// reported as a spend.
#[tokio::test(flavor = "multi_thread")]
async fn split_reorg_with_coins_spent_elsewhere_needs_a_new_step1() {
    let scan = Scan::new(Shape::Wpkh);
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let mut panel = tracking(&scan, &connect, &temp).await;
    refresh(
        &mut panel,
        &connect,
        Status::Observation(Assessment::Reorged),
    )
    .await;

    // Nothing spent: the refusal is shown and the panel keeps tracking.
    let task = panel.update(SplitMessage::CheckReorg);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Tracking);
    assert_eq!(panel.notice(), Some(step1::REORGED));
    assert!(panel.is_bound());

    // Step 1 itself back in the mempool is not a double spend.
    let tracked = panel.tracked_txid().unwrap();
    connect
        .chains
        .spend_on(ChainId::Bitcoin, scan.coins[0].outpoint);
    connect.chains.status.lock().unwrap().insert(
        (ChainId::Bitcoin, tracked),
        TransactionObservation::Unconfirmed { txid: tracked },
    );
    let task = panel.update(SplitMessage::CheckReorg);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Tracking);

    // Absent, and a claimed coin spent on Bitcoin by something else.
    connect
        .chains
        .status
        .lock()
        .unwrap()
        .remove(&(ChainId::Bitcoin, tracked));
    let task = panel.update(SplitMessage::CheckReorg);
    drive(&mut panel, task).await;
    assert!(
        matches!(panel.stage(), Stage::Refused(r) if r.reason == step1::NEW_POISON_NEEDED && !r.retry),
        "{:?}",
        panel.stage()
    );
    assert_eq!(connect.calls.recovers.load(Ordering::SeqCst), 3);
    assert_eq!(connect.calls.resends.load(Ordering::SeqCst), 0);
    assert_eq!(step1::discover(&temp.root()).len(), 1);
}

/// (#626 F1) After a reorg, a failed or stale Bitcoin read is never taken
/// as a spend: neither step 1's own status read nor a claimed coin's
/// unspent-output read. Each gives a retry refusal, and the panel keeps
/// tracking instead of asking for a new step 1. The status cases run with a
/// claimed coin really absent from the unspent outputs (as when step 1
/// itself is back in the mempool), so a status failure read as "absent"
/// would wrongly conclude a double spend.
#[tokio::test(flavor = "multi_thread")]
async fn split_reorg_read_failures_are_never_reported_as_a_spend() {
    let scan = Scan::new(Shape::Wpkh);
    let connect = FakeConnect::new(&scan.coins);
    let temp = Temp::new();
    let mut panel = tracking(&scan, &connect, &temp).await;
    let tracked = panel.tracked_txid().unwrap();
    let claimed = panel.claimed.clone();
    refresh(
        &mut panel,
        &connect,
        Status::Observation(Assessment::Reorged),
    )
    .await;
    // The coordinator stays the one opened through `first`; each case swaps
    // in a Connect (same account) whose Bitcoin reads are faulted.
    let first = connect;

    let cases = [
        ("unspent read error", None, Some(Fault::Error), false),
        ("stale unspent read", None, Some(Fault::Stale), false),
        ("step 1 status read error", Some(Fault::Error), None, true),
        ("stale step 1 status read", Some(Fault::Stale), None, true),
    ];
    for (case, status_fault, utxo_fault, coin_gone) in cases {
        let connect = FakeConnect::new(&scan.coins);
        if coin_gone {
            connect
                .chains
                .spend_on(ChainId::Bitcoin, scan.coins[0].outpoint);
        }
        *connect.chains.status_fault.lock().unwrap() = status_fault;
        *connect.chains.utxo_fault.lock().unwrap() = utxo_fault;

        let refusal = step1::step1_double_spent(&*connect, tracked, &claimed)
            .await
            .expect_err(case);
        assert!(refusal.retry, "{}", case);
        assert!(
            refusal.reason.contains("not a sign that a coin was spent"),
            "{case}: {}",
            refusal.reason
        );

        panel.set_connect(Some(connect.clone() as Arc<dyn SplitConnect>));
        assert!(panel.is_bound() && panel.reorged(), "{}", case);
        let before = first.calls.recovers.load(Ordering::SeqCst);
        let task = panel.update(SplitMessage::CheckReorg);
        drive(&mut panel, task).await;
        assert_eq!(
            panel.stage(),
            &Stage::Tracking,
            "{case}: {:?}",
            panel.stage()
        );
        let notice = panel.notice().unwrap_or_default();
        assert!(
            notice.contains("not a sign that a coin was spent"),
            "{}: {}",
            case,
            notice
        );
        assert_ne!(notice, step1::NEW_POISON_NEEDED, "{}", case);
        assert_eq!(
            first.calls.recovers.load(Ordering::SeqCst),
            before + 1,
            "{case}"
        );

        // Control: the same chain state without the fault.
        *connect.chains.status_fault.lock().unwrap() = None;
        *connect.chains.utxo_fault.lock().unwrap() = None;
        assert_eq!(
            step1::step1_double_spent(&*connect, tracked, &claimed)
                .await
                .unwrap(),
            coin_gone,
            "{case}"
        );
    }
}
