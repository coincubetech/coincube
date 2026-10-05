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
    /// #568 S4b, O4: Bitcoin's unspent outputs by address (initially the
    /// claimed coins); successive Bitcoin reads of step 1 that override
    /// `step1_status`; a fault on the Bitcoin unspent reads only; a
    /// previous transaction served for another txid; and every read, in
    /// order, as `(chain, what)`.
    bitcoin_utxos: Mutex<HashMap<String, BTreeSet<OutPoint>>>,
    step1_reads: Mutex<VecDeque<TransactionObservation>>,
    bitcoin_unspent_fault: Mutex<Option<Fault>>,
    /// A fault on the Bitcoin reads of step 1 only.
    step1_fault: Mutex<Option<Fault>>,
    tampered_previous: Mutex<bool>,
    reads: Mutex<Vec<(ChainId, &'static str)>>,
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
            bitcoin_utxos: Mutex::new(utxos.clone()),
            step1_reads: Mutex::default(),
            bitcoin_unspent_fault: Mutex::default(),
            step1_fault: Mutex::default(),
            tampered_previous: Mutex::default(),
            reads: Mutex::default(),
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
    /// O4's chain state: step 1 in no Bitcoin block and not waiting to be
    /// mined, `outpoint` spent on Bitcoin by another transaction.
    fn conflict_on_bitcoin(&self, outpoint: OutPoint) {
        *self.step1_status.lock().unwrap() = TransactionObservation::Absent;
        for set in self.bitcoin_utxos.lock().unwrap().values_mut() {
            set.remove(&outpoint);
        }
    }
    fn unspend_on_bitcoin(&self, outpoint: OutPoint) {
        let previous = &self.previous[&outpoint.txid];
        let address = Address::from_script(
            &previous.output[outpoint.vout as usize].script_pubkey,
            Network::Bitcoin,
        )
        .unwrap()
        .to_string();
        self.bitcoin_utxos
            .lock()
            .unwrap()
            .entry(address)
            .or_default()
            .insert(outpoint);
    }
    fn take_reads(&self) -> Vec<(ChainId, &'static str)> {
        std::mem::take(&mut *self.reads.lock().unwrap())
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
        self.reads.lock().unwrap().push((chain, "transaction"));
        let (value, fault) = match chain {
            ChainId::Bitcoin => {
                assert_eq!(txid, self.step1);
                let queued = self.step1_reads.lock().unwrap().pop_front();
                (
                    queued.unwrap_or(*self.step1_status.lock().unwrap()),
                    self.step1_fault.lock().unwrap().or(self.fault(chain)),
                )
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
        chain: ChainId,
        txid: Txid,
    ) -> Result<Transaction, FailureKind> {
        self.reads.lock().unwrap().push((chain, "previous"));
        let mut previous = self
            .previous
            .get(&txid)
            .cloned()
            .ok_or(FailureKind::Http(404))?;
        if *self.tampered_previous.lock().unwrap() {
            previous.output[0].value = coincube_core::miniscript::bitcoin::Amount::from_sat(1);
        }
        Ok(previous)
    }
    async fn unspent_outputs(
        &self,
        chain: ChainId,
        address: &str,
    ) -> Result<FreshRead<Vec<OutPoint>>, FailureKind> {
        self.reads.lock().unwrap().push((chain, "unspent"));
        let (utxos, fault) = match chain {
            ChainId::Bitcoin => (
                &self.bitcoin_utxos,
                self.bitcoin_unspent_fault
                    .lock()
                    .unwrap()
                    .or(self.fault(chain)),
            ),
            _ => (&self.btcb2_utxos, self.fault(chain)),
        };
        let set = utxos
            .lock()
            .unwrap()
            .get(address)
            .cloned()
            .unwrap_or_default();
        read(chain, set.into_iter().collect(), fault)
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
    after: Arc<Mutex<Step1AfterStep2>>,
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
    ) -> Result<(Status, TransactionObservation, Step1AfterStep2), Step2Refusal> {
        Ok((
            Status::Observation(Assessment::ObservationsEligibleForPreflight),
            *self.seen.lock().unwrap(),
            *self.after.lock().unwrap(),
        ))
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
struct LockedPort {
    directory: PathBuf,
    digest: sha256::Hash,
    opened: AtomicUsize,
    seen: Arc<Mutex<TransactionObservation>>,
    /// What the reconciles report of step 1 (eligible unless a test says).
    after: Arc<Mutex<Step1AfterStep2>>,
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
            after: self.after.clone(),
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
            after: Arc::new(Mutex::new(Step1AfterStep2::Eligible)),
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

/// Only the BTCB2 unspent reads are stale; every other read is fresh.
struct StaleUnspent(Arc<Chains>);
#[async_trait]
impl SplitEvidenceSource for StaleUnspent {
    fn now(&self) -> i64 {
        self.0.now()
    }
    async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        self.0.tip(chain).await
    }
    async fn transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        self.0.transaction(chain, txid).await
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        self.0.hash_at_height(chain, height).await
    }
    async fn previous_transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<Transaction, FailureKind> {
        self.0.previous_transaction(chain, txid).await
    }
    async fn unspent_outputs(
        &self,
        chain: ChainId,
        address: &str,
    ) -> Result<FreshRead<Vec<OutPoint>>, FailureKind> {
        let value = self
            .0
            .unspent_outputs(chain, address)
            .await?
            .value()
            .clone();
        read(chain, value, Some(Fault::Stale))
    }
}
struct StaleUnspentConnect(StaleUnspent);
#[async_trait]
impl SplitConnect for StaleUnspentConnect {
    fn context(&self) -> Context {
        context()
    }
    fn evidence(&self) -> &dyn SplitEvidenceSource {
        &self.0
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
        unreachable!()
    }
}

/// #644 G3: the two `check_close` retain branches no other case isolates.
/// A stale BTCB2 unspent read alone refuses as unavailable (the stale case
/// above also makes the first step-2 read stale, which refuses first), and
/// a previous transaction that is not the one its outpoint names refuses as
/// unidentified. Adapted from Gimli's #644 review probes.
#[tokio::test(flavor = "multi_thread")]
async fn check_close_refuses_a_stale_unspent_read_and_an_unauthenticated_previous_tx() {
    let fixture = Fixture::new();
    let panel = fixture.panel().await;
    let dead_end = panel.dead_end().cloned().unwrap();
    drop(panel);
    // Clean and fresh: passes.
    assert!(check_close(&Evidence(fixture.chains.clone()), &dead_end)
        .await
        .is_ok());

    // Only the unspent reads stale: refused as unavailable.
    let refused = check_close(
        &StaleUnspentConnect(StaleUnspent(fixture.chains.clone())),
        &dead_end,
    )
    .await
    .expect_err("a stale BTCB2 unspent read was accepted");
    assert!(refused.retry, "{:?}", refused);
    assert!(
        refused.reason.contains("not a sign that step 2 left"),
        "{:?}",
        refused
    );

    // A claimed input's previous transaction swapped for another one.
    let mut chains = Chains::new(&fixture.journal);
    let (first, second) = (dead_end.claimed[0], dead_end.claimed[1]);
    assert_ne!(first.txid, second.txid);
    let other = chains.previous[&second.txid].clone();
    chains.previous.insert(first.txid, other);
    let refused = check_close(&Evidence(Arc::new(chains)), &dead_end)
        .await
        .expect_err("an unauthenticated previous transaction was accepted");
    assert!(!refused.retry, "{:?}", refused);
    assert!(
        refused.reason.contains(step1::UNIDENTIFIED),
        "{:?}",
        refused
    );
}

/// #568 S4b (Legolas F4, applied to the #625 close): the step-2 dead end is
/// offered for closing only while this session's last reconcile found step
/// 1 eligible. The close's own check needs step 1 six deep in its block, so
/// beside any other outcome it could only refuse; a check asked for then
/// does nothing.
#[tokio::test(flavor = "multi_thread")]
async fn panel_offers_the_dead_end_close_only_while_step1_is_eligible() {
    let fixture = Fixture::new();
    let mut panel = fixture.panel().await;
    assert!(panel.can_check_close());
    for after in [
        Step1AfterStep2::Shallow { confirmations: 4 },
        Step1AfterStep2::InMempool,
        Step1AfterStep2::Missing,
        Step1AfterStep2::Unknown,
    ] {
        *fixture.port.after.lock().unwrap() = after;
        let task = panel.update(SplitMessage::Step2Reconcile);
        drive(&mut panel, task).await;
        assert!(panel.dead_end().is_some(), "{:?}", after);
        assert!(!panel.can_check_close(), "{:?}", after);
        check(&mut panel).await;
        assert!(!panel.can_confirm_close(), "{:?}", after);
        assert_eq!(panel.stage(), &Stage::Step2(Step2Stage::Reconcile));
    }
    *fixture.port.after.lock().unwrap() = Step1AfterStep2::Eligible;
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert!(panel.can_check_close());
    check(&mut panel).await;
    assert!(panel.can_confirm_close(), "{:?}", panel.notice());
    assert!(!fixture.tombstone().exists());
}

/// #568 S4b, O4: record a step-1 conflict on the first claimed coin in
/// `journal`, provisional or terminal (six blocks above where it was first
/// seen), as the step-2 reconciler leaves it.
pub(super) fn record_conflict(journal: &Journal, terminal: bool) -> Step1Conflict {
    let first = BlockRef {
        height: 106,
        hash: hash(0x40),
    };
    let mut conflict = Step1Conflict::new(journal.step1.claimed_prevouts()[0], first);
    if terminal {
        conflict = conflict
            .terminal(BlockRef {
                height: first.height + MIN_CONFIRMATIONS,
                hash: hash(0x41),
            })
            .unwrap();
    }
    let path = journal.temp.0.join("intent.json");
    let mut intent: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    intent["split"]["step1_conflict"] = serde_json::to_value(conflict).unwrap();
    std::fs::write(&path, serde_json::to_vec(&intent).unwrap()).unwrap();
    assert_eq!(journal.lock().split_step1_conflict(), Some(conflict));
    conflict
}
impl Fixture {
    /// O4: [`Self::new`] with a terminal (or provisional) step-1 conflict
    /// recorded and Bitcoin showing it: step 1 in no block, the coin spent.
    /// Step 2 is confirmed on BTCB2: its bytes stand.
    fn conflicted(terminal: bool) -> (Self, Step1Conflict) {
        let fixture = Self::new();
        let conflict = record_conflict(&fixture.journal, terminal);
        fixture.chains.conflict_on_bitcoin(conflict.outpoint());
        *fixture.port.seen.lock().unwrap() = TransactionObservation::Confirmed {
            txid: Txid::from_byte_array([5; 32]),
            block: BlockRef {
                height: 1_000,
                hash: hash(0x66),
            },
        };
        *fixture.port.after.lock().unwrap() = Step1AfterStep2::Conflict(conflict);
        (fixture, conflict)
    }
}

/// #568 S4b (Legolas F3, probe P-A): a recorded *terminal* step-1 conflict
/// is a dead end. A restart opens the reconciler in it, never the resend
/// coordinator, even when the journal still records a resend permission
/// (its last send came back refused): the service refuses every resend
/// while the conflict stands. A provisional conflict is no dead end: that
/// journal reopens the coordinator as before.
#[tokio::test(flavor = "multi_thread")]
async fn restart_opens_the_o4_dead_end_instead_of_a_resend() {
    async fn run(journal: &Journal, port: &Arc<Port>) -> Result<Restart, Step2Refusal> {
        restart(
            context(),
            Some(port.clone()),
            Some(resend_ports(journal, port)),
            journal.temp.0.clone(),
            TARGET.into(),
            journal.digest(),
        )
        .await
    }
    let returned = Journal::returned(false);
    let conflict = record_conflict(&returned, true);
    {
        let controller = returned.lock();
        assert!(controller.split_step2_returned() && !controller.split_step2_dead_end());
    }
    let ports = port(&returned);
    match run(&returned, &ports).await {
        Ok(Restart::Reconcile(_, Some(dead_end), None)) => {
            assert_eq!(dead_end.conflict, Some(conflict));
            assert_eq!(dead_end.claimed, returned.step1.claimed_prevouts());
        }
        _ => panic!("a terminal conflict is a dead end"),
    }
    assert_eq!(ports.uncertain.load(Ordering::SeqCst), 0);
    assert_eq!(ports.reconcilers.load(Ordering::SeqCst), 1);

    let provisional = Journal::returned(false);
    record_conflict(&provisional, false);
    let ports = port(&provisional);
    assert!(matches!(
        run(&provisional, &ports).await,
        Ok(Restart::Resend(_))
    ));
    assert_eq!(ports.uncertain.load(Ordering::SeqCst), 1);
}

/// #568 S4b, O4: the close of a terminal step-1 conflict, through the
/// panel. It is offered once a reconcile reports that conflict, whatever
/// step 2 shows on BTCB2, and its check reads Bitcoin only, in order: step
/// 1 absent, the coin's previous transaction, the coin's address's unspent
/// outputs, step 1 absent again; never step 1's depth. The close writes the
/// tombstone naming the coin and leaves the journal, recorded step 2 and
/// conflict included. A restart then opens nothing.
#[tokio::test(flavor = "multi_thread")]
async fn panel_closes_a_split_with_a_terminal_step1_conflict_after_a_bitcoin_check() {
    let (fixture, conflict) = Fixture::conflicted(true);
    let mut panel = fixture.restarted().await;
    assert_eq!(panel.stage(), &Stage::Step2(Step2Stage::Reconcile));
    assert_eq!(panel.dead_end().and_then(|d| d.conflict), Some(conflict));
    // Not before a reconcile reported the conflict.
    assert!(!panel.can_check_close());
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert!(matches!(
        panel.step2_seen(),
        Some(TransactionObservation::Confirmed { .. })
    ));
    assert!(panel.dead_end().is_some());
    assert!(panel.can_check_close() && !panel.can_confirm_close());
    assert!(!panel.can_review_resend() && !panel.can_complete());
    let copy = conflict_close_copy(&conflict);
    assert!(copy.contains(&conflict.outpoint().to_string()), "{}", copy);
    assert!(
        copy.contains("can't complete") && copy.contains("stand"),
        "{}",
        copy
    );
    assert!(!copy.contains("fingerprint"), "{}", copy);

    fixture.chains.take_reads();
    let before = fixture.intent();
    check(&mut panel).await;
    assert!(panel.can_confirm_close(), "{:?}", panel.notice());
    assert_eq!(
        fixture.chains.take_reads(),
        [
            (ChainId::Bitcoin, "transaction"),
            (ChainId::Bitcoin, "previous"),
            (ChainId::Bitcoin, "unspent"),
            (ChainId::Bitcoin, "transaction"),
        ]
    );
    let task = panel.update(SplitMessage::ConfirmAbandon);
    drive(&mut panel, task).await;
    assert_eq!(panel.stage(), &Stage::Closed, "{:?}", panel.notice());
    let tombstone: serde_json::Value =
        serde_json::from_slice(&std::fs::read(fixture.tombstone()).unwrap()).unwrap();
    assert_eq!(tombstone["step1_conflict"], conflict.outpoint().to_string());
    assert_eq!(tombstone["step1_txid"], fixture.chains.step1.to_string());
    assert_eq!(fixture.intent(), before);
    assert_eq!(
        fixture.journal.lock().split_step1_conflict(),
        Some(conflict)
    );

    let opened = fixture.port.opened.load(Ordering::SeqCst);
    let panel = fixture.panel().await;
    assert_eq!(panel.stage(), &Stage::Closed);
    assert_eq!(fixture.port.opened.load(Ordering::SeqCst), opened);
}

/// #568 S4b, O4: nothing is closed, and the check may be retried, when
/// Bitcoin shows step 1 at either read (in a block or waiting to be mined),
/// the conflicting coin is unspent again, a read fails or is stale, or the
/// previous transaction isn't the coin's (final). A provisional conflict is
/// no dead end and offers no close. A reconcile that finds step 1 eligible
/// (S4-D6 cleared the conflict) drops the dead end.
#[tokio::test(flavor = "multi_thread")]
async fn panel_keeps_a_conflicted_split_open_without_clean_fresh_bitcoin_evidence() {
    let (fixture, conflict) = Fixture::conflicted(true);
    let mut panel = fixture.panel().await;
    assert!(panel.can_check_close());
    let coin = conflict.outpoint();
    let seen = TransactionObservation::Unconfirmed {
        txid: fixture.chains.step1,
    };
    type Setup = fn(&Chains, OutPoint, TransactionObservation);
    type Undo = fn(&Chains, OutPoint);
    let cases: [(&str, Setup, Undo, bool); 9] = [
        (
            "step 1 waiting at the first read",
            |c, _, seen| c.step1_reads.lock().unwrap().push_back(seen),
            |_, _| {},
            true,
        ),
        (
            "step 1 in a block at the first read",
            |c, _, _| {
                c.step1_reads
                    .lock()
                    .unwrap()
                    .push_back(TransactionObservation::Confirmed {
                        txid: c.step1,
                        block: step1_block(),
                    })
            },
            |_, _| {},
            true,
        ),
        (
            "step 1 seen at the second read",
            |c, _, seen| {
                let mut reads = c.step1_reads.lock().unwrap();
                reads.push_back(TransactionObservation::Absent);
                reads.push_back(seen);
            },
            |_, _| {},
            true,
        ),
        (
            "coin unspent again",
            |c, coin, _| c.unspend_on_bitcoin(coin),
            |c, coin| c.conflict_on_bitcoin(coin),
            true,
        ),
        (
            "bitcoin read fails",
            |c, _, _| *c.bitcoin_fault.lock().unwrap() = Some(Fault::Error),
            |c, _| *c.bitcoin_fault.lock().unwrap() = None,
            true,
        ),
        (
            "bitcoin read stale",
            |c, _, _| *c.bitcoin_fault.lock().unwrap() = Some(Fault::Stale),
            |c, _| *c.bitcoin_fault.lock().unwrap() = None,
            true,
        ),
        (
            "step-1 read stale",
            |c, _, _| *c.step1_fault.lock().unwrap() = Some(Fault::Stale),
            |c, _| *c.step1_fault.lock().unwrap() = None,
            true,
        ),
        (
            "unspent read stale",
            |c, _, _| *c.bitcoin_unspent_fault.lock().unwrap() = Some(Fault::Stale),
            |c, _| *c.bitcoin_unspent_fault.lock().unwrap() = None,
            true,
        ),
        (
            "previous transaction tampered",
            |c, _, _| *c.tampered_previous.lock().unwrap() = true,
            |c, _| *c.tampered_previous.lock().unwrap() = false,
            false,
        ),
    ];
    let connect = panel.connect.clone().unwrap();
    let dead_end = panel.dead_end().cloned().unwrap();
    for (case, setup, undo, retry) in cases {
        // #658 P3-2: the refusal's own retry flag, which the panel's close
        // offer never reads, is pinned per row.
        setup(&fixture.chains, coin, seen);
        let refusal = check_close(&*connect, &dead_end).await.unwrap_err();
        assert_eq!(refusal.retry, retry, "{}: {}", case, refusal.reason);
        if case == "coin unspent again" {
            // #658 P3-4 (S4b-D1): it names the way out.
            assert_eq!(refusal.reason, conflict_coin_unspent_copy(&coin));
        }
        undo(&fixture.chains, coin);
        fixture.chains.step1_reads.lock().unwrap().clear();
        let _ = fixture.chains.take_reads();

        setup(&fixture.chains, coin, seen);
        check(&mut panel).await;
        assert!(!panel.can_confirm_close(), "{}", case);
        let notice = panel.notice().unwrap_or_default().to_string();
        assert!(!notice.is_empty(), "{}", case);
        if retry {
            assert!(panel.can_check_close(), "{}: {}", case, notice);
        }
        undo(&fixture.chains, coin);
        fixture.chains.step1_reads.lock().unwrap().clear();
        assert!(!fixture.tombstone().exists(), "{}", case);
    }
    // Clean again: the check passes.
    check(&mut panel).await;
    assert!(panel.can_confirm_close(), "{:?}", panel.notice());

    // S4-D6: a reconcile that finds step 1 eligible drops the dead end.
    *fixture.port.after.lock().unwrap() = Step1AfterStep2::Eligible;
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert!(panel.dead_end().is_none());
    assert!(!panel.can_check_close() && !panel.can_confirm_close());
    assert!(!fixture.tombstone().exists());

    // A provisional conflict is no dead end.
    let (fixture, provisional) = Fixture::conflicted(false);
    let panel = fixture.panel().await;
    assert_eq!(
        panel.step2_after(),
        Some(Step1AfterStep2::Conflict(provisional))
    );
    assert!(panel.dead_end().is_none_or(|d| d.conflict.is_none()));
    assert!(!panel.can_check_close());
}

/// #568 S4b, O4: a reconcile that reports a terminal conflict the panel
/// holds no dead end for (it became terminal under this session) reads the
/// journal again, as a restart: the dead end comes with the reconciler and
/// the close is offered without another reconcile.
#[tokio::test(flavor = "multi_thread")]
async fn panel_reads_the_journal_again_when_a_conflict_becomes_terminal() {
    let (fixture, provisional) = Fixture::conflicted(false);
    let mut panel = fixture.panel().await;
    assert!(!panel.can_check_close());
    assert_eq!(fixture.port.opened.load(Ordering::SeqCst), 1);
    // The reconciler holds the journal's lock; the fake's reconcile reports
    // what the real one would have written.
    let path = fixture.journal.temp.0.join("intent.json");
    let terminal = provisional
        .terminal(BlockRef {
            height: provisional.bitcoin_tip().height + MIN_CONFIRMATIONS,
            hash: hash(0x41),
        })
        .unwrap();
    let mut intent: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    intent["split"]["step1_conflict"] = serde_json::to_value(terminal).unwrap();
    std::fs::write(&path, serde_json::to_vec(&intent).unwrap()).unwrap();
    *fixture.port.after.lock().unwrap() = Step1AfterStep2::Conflict(terminal);
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_eq!(fixture.port.opened.load(Ordering::SeqCst), 2);
    assert_eq!(panel.stage(), &Stage::Step2(Step2Stage::Reconcile));
    assert_eq!(panel.dead_end().and_then(|d| d.conflict), Some(terminal));
    assert!(panel.can_check_close());
    // Read once: the next reconcile reports the same conflict and reads
    // nothing again.
    let task = panel.update(SplitMessage::Step2Reconcile);
    drive(&mut panel, task).await;
    assert_eq!(fixture.port.opened.load(Ordering::SeqCst), 2);
    assert!(panel.can_check_close());
}

/// #568 S4b, O4: the close re-reads the journal under its lock. A conflict
/// cleared (or changed) after the check closes nothing.
#[test]
fn close_refuses_a_conflict_cleared_since_the_check() {
    let (fixture, conflict) = Fixture::conflicted(true);
    let dead_end = DeadEnd {
        step1: fixture.chains.step1,
        step2: fixture
            .journal
            .lock()
            .recorded_split_step2()
            .unwrap()
            .compute_txid(),
        claimed: fixture.journal.step1.claimed_prevouts(),
        conflict: Some(conflict),
    };
    let path = fixture.journal.temp.0.join("intent.json");
    let mut intent: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    intent["split"]
        .as_object_mut()
        .unwrap()
        .remove("step1_conflict");
    std::fs::write(&path, serde_json::to_vec(&intent).unwrap()).unwrap();
    let ended = std::sync::atomic::AtomicBool::new(false);
    assert_eq!(
        close(
            &fixture.journal.temp.0,
            TARGET,
            fixture.journal.digest(),
            context(),
            &dead_end,
            now(),
            &ended,
        ),
        Err(CHANGED_SINCE_CHECK.to_string())
    );
    assert!(!fixture.tombstone().exists());
}
