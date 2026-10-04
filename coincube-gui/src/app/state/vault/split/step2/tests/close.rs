//! #625 F2 (A1 = A): closing a split in its step-2 dead end, through the
//! panel, against the real journal and its lock. The reconciler holds that
//! lock like the production one; the chain evidence is synthetic.
use super::*;
use crate::app::{
    message::Message,
    state::vault::split::{SplitMessage, SplitPanel, Stage, Step2Stage},
};
use crate::services::claim_observation::FreshRead;
use coincube_core::miniscript::bitcoin::Address;
use iced::{futures::StreamExt, Task};
use reqwest::header::{HeaderMap, CACHE_CONTROL};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    sync::Mutex,
};

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    Error,
    Stale,
}

fn read<T>(chain: ChainId, value: T, fault: Option<Fault>) -> Result<FreshRead<T>, FailureKind> {
    let observed_at = match fault {
        None => now(),
        Some(Fault::Stale) => now() - 3_600,
        Some(Fault::Error) => return Err(FailureKind::Http(503)),
    };
    let mut headers = HeaderMap::new();
    headers.insert("x-cache", "BYPASS".parse().unwrap());
    headers.insert(CACHE_CONTROL, "no-store".parse().unwrap());
    FreshRead::from_response(chain, value, observed_at, &headers)
}

/// Step 1's Bitcoin block in these fixtures.
const STEP1_HEIGHT: u64 = 100;
fn step1_block() -> BlockRef {
    BlockRef {
        height: STEP1_HEIGHT,
        hash: hash(0x44),
    }
}

/// Both chains around one dead end: step 1 six deep on Bitcoin, the claimed
/// coins unspent on BTCB2, the recorded step 2 absent from BTCB2.
struct Chains {
    step1: Txid,
    previous: HashMap<Txid, Transaction>,
    /// Successive BTCB2 reads of the recorded step 2 (then Absent).
    step2_reads: Mutex<VecDeque<TransactionObservation>>,
    step1_status: Mutex<TransactionObservation>,
    bitcoin_tip: Mutex<u64>,
    canonical: Mutex<BlockHash>,
    btcb2_utxos: Mutex<HashMap<String, BTreeSet<OutPoint>>>,
    /// A fault on every BTCB2 read, and on every Bitcoin read.
    btcb2_fault: Mutex<Option<Fault>>,
    bitcoin_fault: Mutex<Option<Fault>>,
    /// A fault on the BTCB2 reads of the recorded step 2 only.
    step2_fault: Mutex<Option<Fault>>,
}
impl Chains {
    fn new(journal: &Journal) -> Self {
        let step1 = sign1(&journal.step1, &journal.wallet)
            .transaction()
            .compute_txid();
        let mut previous = HashMap::new();
        let mut utxos: HashMap<String, BTreeSet<OutPoint>> = HashMap::new();
        for coin in coins(&journal.wallet) {
            let address = Address::from_script(
                &coin.previous.output[coin.outpoint.vout as usize].script_pubkey,
                Network::Bitcoin,
            )
            .unwrap()
            .to_string();
            utxos.entry(address).or_default().insert(coin.outpoint);
            previous.insert(coin.outpoint.txid, coin.previous);
        }
        Self {
            step1,
            previous,
            step2_reads: Mutex::default(),
            step1_status: Mutex::new(TransactionObservation::Confirmed {
                txid: step1,
                block: step1_block(),
            }),
            bitcoin_tip: Mutex::new(STEP1_HEIGHT + MIN_CONFIRMATIONS - 1),
            canonical: Mutex::new(step1_block().hash),
            btcb2_utxos: Mutex::new(utxos),
            btcb2_fault: Mutex::default(),
            bitcoin_fault: Mutex::default(),
            step2_fault: Mutex::default(),
        }
    }
    fn spend_on_btcb2(&self, outpoint: OutPoint) {
        for set in self.btcb2_utxos.lock().unwrap().values_mut() {
            set.remove(&outpoint);
        }
    }
    fn fault(&self, chain: ChainId) -> Option<Fault> {
        match chain {
            ChainId::Bitcoin => *self.bitcoin_fault.lock().unwrap(),
            _ => *self.btcb2_fault.lock().unwrap(),
        }
    }
}
#[async_trait]
impl SplitEvidenceSource for Chains {
    fn now(&self) -> i64 {
        now()
    }
    async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        assert_eq!(chain, ChainId::Bitcoin);
        let height = *self.bitcoin_tip.lock().unwrap();
        read(
            chain,
            BlockRef {
                height,
                hash: hash(0x55),
            },
            self.fault(chain),
        )
    }
    async fn transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        let (value, fault) = match chain {
            ChainId::Bitcoin => {
                assert_eq!(txid, self.step1);
                (*self.step1_status.lock().unwrap(), self.fault(chain))
            }
            _ => (
                self.step2_reads
                    .lock()
                    .unwrap()
                    .pop_front()
                    .unwrap_or(TransactionObservation::Absent),
                self.step2_fault.lock().unwrap().or(self.fault(chain)),
            ),
        };
        read(chain, value, fault)
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        assert_eq!((chain, height), (ChainId::Bitcoin, STEP1_HEIGHT));
        read(chain, *self.canonical.lock().unwrap(), self.fault(chain))
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
        assert_eq!(chain, ChainId::BitcoinBlake2b);
        let set = self
            .btcb2_utxos
            .lock()
            .unwrap()
            .get(address)
            .cloned()
            .unwrap_or_default();
        read(chain, set.into_iter().collect(), self.fault(chain))
    }
}

/// The session's Connect side: only the chain evidence is read.
struct Evidence(Arc<Chains>);
#[async_trait]
impl SplitConnect for Evidence {
    fn context(&self) -> Context {
        context()
    }
    fn evidence(&self) -> &dyn SplitEvidenceSource {
        &*self.0
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
        unreachable!("a recorded step 2 never reopens step 1")
    }
}

/// A reconciler holding the real journal lock, like the production one,
/// whose reconcile reports what the test sets.
struct LockedRecon {
    _controller: Controller,
    seen: Arc<Mutex<TransactionObservation>>,
}
#[async_trait]
impl Step2Recon for LockedRecon {
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
        Ok((
            Status::Observation(Assessment::ObservationsEligibleForPreflight),
            *self.seen.lock().unwrap(),
        ))
    }
}
struct LockedPort {
    directory: PathBuf,
    digest: sha256::Hash,
    opened: AtomicUsize,
    seen: Arc<Mutex<TransactionObservation>>,
}
impl ReconPort for LockedPort {
    fn context(&self) -> Context {
        context()
    }
    fn open_reconciler(
        &self,
        _: PathBuf,
        _: String,
        _: sha256::Hash,
    ) -> Result<Box<dyn Step2Recon>, Step2Refusal> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(LockedRecon {
            _controller: Controller::reopen_settling_blocking(
                &self.directory,
                &claim_workflow::split_identity(TARGET.into(), self.digest),
                context(),
            )
            .map_err(|error| Step2Refusal::retry(format!("{error:?}")))?,
            seen: self.seen.clone(),
        }))
    }
}

async fn drive(panel: &mut SplitPanel, task: Task<Message>) {
    let mut pending = vec![task];
    while let Some(task) = pending.pop() {
        let Some(mut stream) = iced_runtime::task::into_stream(task) else {
            continue;
        };
        while let Some(action) = stream.next().await {
            if let iced_runtime::Action::Output(Message::Split(event)) = action {
                pending.push(panel.apply(*event));
            }
        }
    }
}

struct Fixture {
    journal: Journal,
    chains: Arc<Chains>,
    port: Arc<LockedPort>,
}
impl Fixture {
    fn new() -> Self {
        let journal = Journal::new(true);
        let chains = Arc::new(Chains::new(&journal));
        let port = Arc::new(LockedPort {
            directory: journal.temp.0.clone(),
            digest: journal.digest(),
            opened: AtomicUsize::new(0),
            seen: Arc::new(Mutex::new(TransactionObservation::Absent)),
        });
        Self {
            journal,
            chains,
            port,
        }
    }
    /// A panel restarted on the journal (reconcile only) without a
    /// reconcile yet.
    async fn restarted(&self) -> SplitPanel {
        let mut panel = SplitPanel::resume(
            TARGET.into(),
            self.journal.temp.0.parent().unwrap().to_path_buf(),
            self.journal.digest(),
            self.journal.temp.0.clone(),
        );
        panel.set_connect(Some(Arc::new(Evidence(self.chains.clone()))));
        panel.set_recon_port(Some(self.port.clone()));
        let task = panel.begin();
        drive(&mut panel, task).await;
        panel
    }
    /// [`Self::restarted`], then one reconcile.
    async fn panel(&self) -> SplitPanel {
        let mut panel = self.restarted().await;
        if panel.stage() == &Stage::Step2(Step2Stage::Reconcile) {
            let task = panel.update(SplitMessage::Step2Reconcile);
            drive(&mut panel, task).await;
        }
        panel
    }
    fn tombstone(&self) -> PathBuf {
        self.journal.temp.0.join(step1::CLOSED)
    }
    fn intent(&self) -> Vec<u8> {
        std::fs::read(self.journal.temp.0.join("intent.json")).unwrap()
    }
}

async fn check(panel: &mut SplitPanel) {
    let task = panel.update(SplitMessage::CheckAbandon);
    drive(panel, task).await;
}

/// #625 F2: a dead end is offered for closing from the reconcile-only
/// stage; the check passes on clean chains; the close releases the
/// reconciler's lock, checks again and writes the tombstone, leaving the
/// journal (and the recorded signed step 2) untouched. A closed split opens
/// nothing at restart.
#[tokio::test(flavor = "multi_thread")]
async fn panel_closes_a_step2_dead_end_after_a_check_on_both_chains() {
    let fixture = Fixture::new();
    // Not before a reconcile saw step 2 absent: an accepted send may still
    // be in a mempool.
    let mut panel = fixture.restarted().await;
    assert_eq!(panel.stage(), &Stage::Step2(Step2Stage::Reconcile));
    assert!(panel.dead_end().is_some() && !panel.can_check_close());
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Step2(Step2Stage::Reconcile));
    let dead_end = panel.dead_end().cloned().unwrap();
    assert_eq!(dead_end.step1, fixture.chains.step1);
    assert_eq!(dead_end.claimed, fixture.journal.step1.claimed_prevouts());
    assert!(panel.can_check_close() && !panel.can_confirm_close());
    // The step-1 abandon is not offered: nothing of step 1 is installed.
    assert!(!panel.can_check_abandon());
    // Confirming without a check is ignored.
    let task = panel.update(SplitMessage::ConfirmAbandon);
    drive(&mut panel, task).await;
    assert!(!fixture.tombstone().exists());

    let before = fixture.intent();
    check(&mut panel).await;
    assert!(panel.can_confirm_close(), "{:?}", panel.notice());
    assert_eq!(panel.stage(), &Stage::Step2(Step2Stage::Reconcile));
    let task = panel.update(SplitMessage::ConfirmAbandon);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Closed, "{:?}", panel.stage());
    assert!(panel.dead_end().is_none() && !panel.can_check_close());

    // The tombstone names the split; the journal is unchanged and unlocked.
    let tombstone: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.tombstone()).unwrap()).unwrap();
    assert_eq!(
        tombstone["source_digest"],
        fixture.journal.digest().to_string()
    );
    assert_eq!(tombstone["target_cube"], TARGET);
    assert_eq!(tombstone["step1_txid"], dead_end.step1.to_string());
    assert_eq!(tombstone["step2_txid"], dead_end.step2.to_string());
    assert_eq!(
        tombstone["claimed_prevouts"],
        serde_json::json!(dead_end
            .claimed
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>())
    );
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(fixture.tombstone())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }
    assert_eq!(fixture.intent(), before);
    let reopened = fixture.journal.lock();
    assert_eq!(
        reopened.recorded_split_step2().unwrap().compute_txid(),
        dead_end.step2
    );
    drop(reopened);
    assert!(step1::is_closed(&fixture.journal.temp.0));

    // A restart opens nothing.
    let opened = fixture.port.opened.load(Ordering::SeqCst);
    let mut panel = fixture.panel().await;
    assert_eq!(panel.stage(), &Stage::Closed);
    assert_eq!(fixture.port.opened.load(Ordering::SeqCst), opened);
    for message in [
        SplitMessage::Step2Reconcile,
        SplitMessage::CheckAbandon,
        SplitMessage::ConfirmAbandon,
        SplitMessage::Retry,
    ] {
        let task = panel.update(message);
        drive(&mut panel, task).await;
        assert_eq!(panel.stage(), &Stage::Closed);
    }
    panel.revoke();
    assert_eq!(panel.stage(), &Stage::Closed);
}

/// #625 F2: the check refuses, and nothing is closed, when the recorded
/// step 2 is seen on BTCB2 (in either read), a claimed coin is spent there,
/// step 1 is not six deep or its block is no longer canonical, or a read
/// fails or is stale.
#[tokio::test(flavor = "multi_thread")]
async fn panel_keeps_a_step2_dead_end_open_without_clean_fresh_evidence() {
    let fixture = Fixture::new();
    let mut panel = fixture.panel().await;
    let step2 = panel.dead_end().unwrap().step2;
    let seen = TransactionObservation::Unconfirmed { txid: step2 };
    let chains = &fixture.chains;
    let refused = |panel: &SplitPanel, wanted: &str| {
        assert!(!panel.can_confirm_close(), "{}", wanted);
        assert!(
            panel.notice().is_some_and(|notice| notice.contains(wanted)),
            "{} / {:?}",
            wanted,
            panel.notice()
        );
        assert_eq!(panel.stage(), &Stage::Step2(Step2Stage::Reconcile));
        assert!(!fixture.tombstone().exists());
    };
    let reset = || {
        let fresh = Chains::new(&fixture.journal);
        *chains.step2_reads.lock().unwrap() = VecDeque::new();
        *chains.step1_status.lock().unwrap() = *fresh.step1_status.lock().unwrap();
        *chains.bitcoin_tip.lock().unwrap() = *fresh.bitcoin_tip.lock().unwrap();
        *chains.canonical.lock().unwrap() = *fresh.canonical.lock().unwrap();
        *chains.btcb2_utxos.lock().unwrap() = fresh.btcb2_utxos.lock().unwrap().clone();
        *chains.btcb2_fault.lock().unwrap() = None;
        *chains.bitcoin_fault.lock().unwrap() = None;
        *chains.step2_fault.lock().unwrap() = None;
    };

    // Seen in the first read, then only in the last one.
    chains.step2_reads.lock().unwrap().push_back(seen);
    check(&mut panel).await;
    refused(&panel, STEP2_SEEN);
    reset();
    chains
        .step2_reads
        .lock()
        .unwrap()
        .extend([TransactionObservation::Absent, seen]);
    check(&mut panel).await;
    refused(&panel, STEP2_SEEN);

    // A claimed coin spent on BTCB2.
    reset();
    chains.spend_on_btcb2(fixture.journal.step1.claimed_prevouts()[1]);
    check(&mut panel).await;
    refused(&panel, COIN_SPENT_ON_BTCB2);

    // Step 1 five deep, unconfirmed, or in a block no longer canonical.
    reset();
    *chains.bitcoin_tip.lock().unwrap() -= 1;
    check(&mut panel).await;
    refused(&panel, STEP1_NOT_DEEP);
    reset();
    *chains.step1_status.lock().unwrap() = TransactionObservation::Absent;
    check(&mut panel).await;
    refused(&panel, STEP1_NOT_DEEP);
    reset();
    *chains.canonical.lock().unwrap() = hash(0x45);
    check(&mut panel).await;
    refused(&panel, STEP1_NOT_DEEP);

    // A failed or stale read on either chain, or of the step 2 alone.
    for fault in [Fault::Error, Fault::Stale] {
        for chain_fault in [
            &chains.btcb2_fault,
            &chains.bitcoin_fault,
            &chains.step2_fault,
        ] {
            reset();
            *chain_fault.lock().unwrap() = Some(fault);
            check(&mut panel).await;
            refused(&panel, "not a sign that step 2 left");
        }
    }

    // Clean again: the check passes.
    reset();
    check(&mut panel).await;
    assert!(panel.can_confirm_close(), "{:?}", panel.notice());
}

/// #625 F2: a passing check doesn't carry over a session change, a spend
/// before confirmation, or a failed tombstone write; a reconcile that sees
/// step 2 ends the dead end; a journal whose resend is reviewable is none.
#[tokio::test(flavor = "multi_thread")]
async fn panel_keeps_a_step2_dead_end_open_when_anything_changes_before_closing() {
    let fixture = Fixture::new();

    // The session ends after the check: nothing to confirm until a new
    // check under the next session.
    let mut panel = fixture.panel().await;
    check(&mut panel).await;
    assert!(panel.can_confirm_close());
    panel.revoke();
    assert!(!panel.can_confirm_close() && panel.dead_end().is_none());
    let task = panel.update(SplitMessage::ConfirmAbandon);
    drive(&mut panel, task).await;
    assert!(!fixture.tombstone().exists());
    drop(panel);

    // The session ends after the close was confirmed, before its task
    // writes (#644 r4176212750): nothing is closed.
    let mut panel = fixture.panel().await;
    check(&mut panel).await;
    assert!(panel.can_confirm_close());
    let task = panel.update(SplitMessage::ConfirmAbandon);
    panel.revoke();
    drive(&mut panel, task).await;
    assert!(!fixture.tombstone().exists());
    assert!(!step1::is_closed(&fixture.journal.temp.0));
    assert_eq!(panel.stage(), &Stage::NeedsSession);
    drop(panel);

    // A coin spent between the check and the confirmation: the close checks
    // again and refuses; a retry reopens the reconciler.
    let mut panel = fixture.panel().await;
    check(&mut panel).await;
    assert!(panel.can_confirm_close());
    fixture
        .chains
        .spend_on_btcb2(fixture.journal.step1.claimed_prevouts()[0]);
    let task = panel.update(SplitMessage::ConfirmAbandon);
    drive(&mut panel, task).await;
    assert!(
        matches!(panel.stage(), Stage::Refused(r) if r.retry && r.reason == COIN_SPENT_ON_BTCB2),
        "{:?}",
        panel.stage()
    );
    assert!(!fixture.tombstone().exists());
    let task = panel.update(SplitMessage::Retry);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Step2(Step2Stage::Reconcile));
    assert!(panel.dead_end().is_some());
    drop(panel);

    // The tombstone can't be written: nothing is closed.
    let fixture = Fixture::new();
    let mut panel = fixture.panel().await;
    check(&mut panel).await;
    let before = fixture.intent();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &fixture.journal.temp.0,
            std::fs::Permissions::from_mode(0o500),
        )
        .unwrap();
    }
    let task = panel.update(SplitMessage::ConfirmAbandon);
    drive(&mut panel, task).await;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            &fixture.journal.temp.0,
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    assert!(
        matches!(panel.stage(), Stage::Refused(r) if r.retry && r.reason.contains("could not be abandoned")),
        "{:?}",
        panel.stage()
    );
    assert!(!fixture.tombstone().exists());
    assert!(!step1::is_closed(&fixture.journal.temp.0));
    assert_eq!(fixture.intent(), before);
    drop(panel);

    // A reconcile that sees step 2 on BTCB2: it left, no dead end any more.
    let mut panel = fixture.panel().await;
    assert!(panel.can_check_close());
    let step2 = panel.dead_end().unwrap().step2;
    *fixture.port.seen.lock().unwrap() = TransactionObservation::Unconfirmed { txid: step2 };
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert!(panel.dead_end().is_none() && !panel.can_check_close());
    drop(panel);

    // A journal whose last attempt's return is recorded can still be
    // resent: it is no dead end, and nothing is offered.
    let fixture = Fixture::new();
    fixture
        .journal
        .lock()
        .record_split_step2_returned(&context())
        .unwrap();
    let panel = fixture.panel().await;
    assert_eq!(panel.stage(), &Stage::Step2(Step2Stage::Reconcile));
    assert!(panel.dead_end().is_none() && !panel.can_check_close());
}

/// #625 F2: the close re-reads the journal under its lock and refuses a
/// journal that is no longer that dead end; it never deletes the journal.
#[test]
fn close_refuses_a_journal_that_changed_since_the_check() {
    let journal = Journal::new(true);
    let dead_end = {
        let controller = journal.lock();
        dead_end(&controller).unwrap()
    };
    let ended = std::sync::atomic::AtomicBool::new(false);
    let close_now = |dead_end: &DeadEnd, ended: &std::sync::atomic::AtomicBool| {
        close(
            &journal.temp.0,
            TARGET,
            journal.digest(),
            context(),
            dead_end,
            now(),
            ended,
        )
    };
    // The session ended after the close was confirmed (#644 r4176212750):
    // refused under the lock, nothing written.
    assert_eq!(
        close_now(&dead_end, &std::sync::atomic::AtomicBool::new(true)),
        Err(step1::ENDED_BEFORE_ABANDON.to_string())
    );
    assert!(!step1::is_closed(&journal.temp.0));
    // Another step 2 than the one checked.
    let mut other = dead_end.clone();
    other.step2 = Txid::from_byte_array([9; 32]);
    assert_eq!(
        close_now(&other, &ended),
        Err(CHANGED_SINCE_CHECK.to_string())
    );
    // A resend became reviewable.
    journal
        .lock()
        .record_split_step2_returned(&context())
        .unwrap();
    assert_eq!(
        close_now(&dead_end, &ended),
        Err(CHANGED_SINCE_CHECK.to_string())
    );
    assert!(!step1::is_closed(&journal.temp.0));
    assert!(journal.temp.0.join("intent.json").exists());
}

/// #644 G2: a restart that waits for the journal's lock while a close holds
/// it opens nothing once the close wrote its tombstone: it reads the
/// tombstone again under the lock. Adapted from Gimli's #644 review probe.
#[tokio::test(flavor = "multi_thread")]
async fn restart_waiting_on_a_close_opens_nothing() {
    let fixture = Fixture::new();
    let directory = fixture.journal.temp.0.clone();
    // The close's critical section: the journal lock held.
    let held = fixture.journal.lock();
    let port: Arc<dyn ReconPort> = fixture.port.clone();
    let restarting = tokio::spawn(restart(
        context(),
        Some(port),
        None,
        directory.clone(),
        TARGET.into(),
        fixture.journal.digest(),
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!restarting.is_finished());
    // The close writes its tombstone under the lock, then releases it.
    std::fs::write(directory.join(step1::CLOSED), b"{}").unwrap();
    drop(held);
    assert!(matches!(restarting.await.unwrap(), Ok(Restart::Closed)));
    assert_eq!(fixture.port.opened.load(Ordering::SeqCst), 0);
    // The journal is not left locked.
    drop(fixture.journal.lock());
}

/// #644 G1: the close is offered only from a reconcile under this session.
/// A new session resuming the same panel keeps the last BTCB2 observation
/// for its warning, but offers the close only after its own reconcile saw
/// step 2 absent. Adapted from Gimli's #644 review probe.
#[tokio::test(flavor = "multi_thread")]
async fn panel_offers_the_close_only_from_this_sessions_reconcile() {
    let fixture = Fixture::new();
    let mut panel = fixture.panel().await;
    assert!(panel.can_check_close());
    // The session ends, then a new one resumes the panel.
    panel.set_connect(None);
    assert!(!panel.can_check_close());
    panel.set_connect(Some(Arc::new(Evidence(fixture.chains.clone()))));
    let task = panel.begin();
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Step2(Step2Stage::Reconcile));
    assert!(panel.dead_end().is_some());
    assert_eq!(panel.step2_seen(), Some(TransactionObservation::Absent));
    assert!(!panel.can_check_close());
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert!(panel.can_check_close());
}
