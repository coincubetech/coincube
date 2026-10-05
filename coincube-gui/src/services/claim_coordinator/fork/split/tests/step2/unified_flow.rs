//! Split (#568 B4b-3a) the unified fallback's services end to end: the
//! fork-only observation path (U1), the coordinator's C2 gate, its
//! confirmation (U3, U4) on both routes, restart of an unsubmitted record,
//! and the fork-only reconciler (U2). A real P2PKH foreign wallet signed
//! `ALL|UNIFIED` by a session signer and verified by core; a synthetic BTCB2
//! view whose every Bitcoin observation read panics; step 2's real routes
//! over a substitute target-Vault daemon; Connect's preflight and a Knots
//! node mocked over HTTP.
use super::unified_journal::{
    fork_only_journal, sign_unified, unified_coins, unified_sweep, unified_wallet, UnifiedWallet,
};
use super::*;
use crate::services::{
    claim_coordinator::fork::split::step2::{
        Step2Daemon, Step2Routes, UnifiedCoordinator, UnifiedError, UnifiedReconciler,
    },
    claim_observation::{collect_fork_sweep, Failure, Stage},
};
use coincube_core::foreign_split::{UnifiedReplayStatus, VerifiedUnifiedSweep};
use coincubed::config::{BitcoindConfig, BitcoindRpcAuth};

/// The BTCB2 tip the view serves; its hash is `hash(2)`, the tip the mocked
/// preflights answer at.
const BTCB2_TIP: u64 = 1_000;

type Hook = Box<dyn FnOnce(&mut Fork) + Send>;

/// The synthetic BTCB2 side, and the target's history on both chains.
struct Fork {
    tip: BlockRef,
    /// The fork activation height the anchor names.
    fork_height: u64,
    /// Seconds subtracted from the anchor's stamp.
    anchor_age: i64,
    /// Transactions seen on BTCB2, by txid; anything else is absent.
    seen: Vec<(Txid, TransactionObservation)>,
    /// Answered once, by the next BTCB2 read of that transaction.
    seen_once: Option<(Txid, TransactionObservation)>,
    unspent: BTreeSet<OutPoint>,
    /// Addresses with history, per chain.
    used: Vec<(ChainId, String)>,
    /// Applied once, at the next BTCB2 unspent read: between a review's two
    /// collections.
    between: Option<Hook>,
    /// Applied once, at the next BTCB2 transaction read: inside one
    /// collection, between its two anchor reads.
    inside: Option<Hook>,
    anchor_reads: usize,
}
#[derive(Clone)]
struct ForkChains(Arc<Mutex<Fork>>);
impl ForkChains {
    fn new(unspent: BTreeSet<OutPoint>) -> Self {
        Self(Arc::new(Mutex::new(Fork {
            tip: BlockRef {
                height: BTCB2_TIP,
                hash: hash(2),
            },
            fork_height: FORK,
            anchor_age: 0,
            seen: Vec::new(),
            seen_once: None,
            unspent,
            used: Vec::new(),
            between: None,
            inside: None,
            anchor_reads: 0,
        })))
    }
    fn edit(&self, edit: impl FnOnce(&mut Fork)) {
        edit(&mut self.0.lock().unwrap());
    }
}
#[async_trait]
impl ObservationSource for ForkChains {
    fn now(&self) -> i64 {
        now()
    }
    async fn anchor(&self, chain: ChainId) -> Result<NetworkAnchorStatus, FailureKind> {
        assert_eq!(chain, ChainId::BitcoinBlake2b, "a fork-only anchor read");
        let mut view = self.0.lock().unwrap();
        view.anchor_reads += 1;
        Ok(NetworkAnchorStatus {
            network: chain,
            state: AnchorState::Available,
            anchor: Some(NetworkAnchor {
                tip_hash: view.tip.hash,
                tip_height: view.tip.height,
                tip_median_time_past: MTP,
                observed_at: now() - view.anchor_age,
                observation: NetworkObservation {
                    tip_height: view.tip.height,
                    fork: Some(ForkActivation {
                        height: view.fork_height,
                        active: true,
                    }),
                    rdts: RdtsStatus::Flagday {
                        flagday: RdtsFlagday {
                            height: 90,
                            expiry_time: 20_000,
                            active: true,
                        },
                    },
                },
            }),
        })
    }
    async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        panic!("the fork-only path read the {:?} tip", chain);
    }
    async fn transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        assert_eq!(
            chain,
            ChainId::BitcoinBlake2b,
            "the fork-only path read Bitcoin"
        );
        let mut view = self.0.lock().unwrap();
        if let Some(inside) = view.inside.take() {
            inside(&mut view);
        }
        if view.seen_once.is_some_and(|(id, _)| id == txid) {
            let (_, once) = view.seen_once.take().unwrap();
            return Chains::read(chain, once);
        }
        let seen = view
            .seen
            .iter()
            .find(|(id, _)| *id == txid)
            .map(|(_, seen)| *seen)
            .unwrap_or(TransactionObservation::Absent);
        Chains::read(chain, seen)
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        assert_eq!(
            chain,
            ChainId::BitcoinBlake2b,
            "the fork-only path read Bitcoin"
        );
        let view = self.0.lock().unwrap();
        let block = view.seen.iter().find_map(|(_, seen)| match seen {
            TransactionObservation::Confirmed { block, .. } if block.height == height => {
                Some(block.hash)
            }
            _ => None,
        });
        Chains::read(
            chain,
            match block {
                Some(hash) => hash,
                None if height == view.tip.height => view.tip.hash,
                None => hash(0x44),
            },
        )
    }
}
#[async_trait]
impl SplitForkServices for ForkChains {
    fn source(&self) -> &dyn ObservationSource {
        self
    }
    fn origin(&self) -> &str {
        ORIGIN
    }
    async fn btcb2_unspent(&self, _: &str) -> Result<FreshRead<Vec<OutPoint>>, FailureKind> {
        let mut view = self.0.lock().unwrap();
        if let Some(between) = view.between.take() {
            between(&mut view);
        }
        Chains::read(
            ChainId::BitcoinBlake2b,
            view.unspent.iter().copied().collect(),
        )
    }
    async fn address_used(
        &self,
        chain: ChainId,
        address: &str,
    ) -> Result<FreshRead<bool>, FailureKind> {
        let view = self.0.lock().unwrap();
        Chains::read(chain, view.used.contains(&(chain, address.to_owned())))
    }
}

/// One recorded send: route, txid, target index, and whether the journal
/// held its submission intent when the send began.
type Sent = (&'static str, Txid, ChildNumber, bool);

/// A target Vault daemon on either route whose backend binding a test can
/// switch; it refuses a binding other than the current one, as the real
/// daemon does, and answers every send `accept`ed or refused.
#[derive(Clone)]
struct UnifiedVault {
    binding: Arc<AtomicUsize>,
    node: Arc<Mutex<Option<BitcoindConfig>>>,
    sends: Arc<Mutex<Vec<Sent>>>,
    journal: PathBuf,
    accept: Arc<std::sync::atomic::AtomicBool>,
}
impl UnifiedVault {
    fn new(node: Option<BitcoindConfig>, journal: PathBuf) -> Self {
        Self {
            binding: Arc::new(AtomicUsize::new(1)),
            node: Arc::new(Mutex::new(node)),
            sends: Arc::new(Mutex::new(Vec::new())),
            journal,
            accept: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
    fn send(
        &self,
        route: &'static str,
        verified: &VerifiedUnifiedSweep,
        target: ChildNumber,
        binding: usize,
    ) -> Result<SubmissionOutcome, DaemonError> {
        if binding != self.binding.load(Ordering::SeqCst) {
            return Err(DaemonError::PoisonSubmission(
                coincubed::poison_broadcast::SubmissionError::BackendUnavailable,
            ));
        }
        let tx = verified.transaction();
        let intent = std::fs::read(self.journal.join("intent.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .is_some_and(|journal| {
                journal["fork_submission"]["txid"] == tx.compute_txid().to_string()
            });
        self.sends
            .lock()
            .unwrap()
            .push((route, tx.compute_txid(), target, intent));
        if !self.accept.load(Ordering::SeqCst) {
            return Err(DaemonError::PoisonSubmission(
                coincubed::poison_broadcast::SubmissionError::BackendUnavailable,
            ));
        }
        Ok(SubmissionOutcome::UpstreamAccepted {
            txid: tx.compute_txid(),
            wtxid: tx.compute_wtxid(),
        })
    }
    fn sends(&self) -> Vec<Sent> {
        self.sends.lock().unwrap().clone()
    }
}
#[async_trait]
impl Step2Daemon for UnifiedVault {
    type Binding = usize;
    async fn binding(&self) -> Result<usize, DaemonError> {
        Ok(self.binding.load(Ordering::SeqCst))
    }
    fn node(&self) -> Option<BitcoindConfig> {
        self.node.lock().unwrap().clone()
    }
    async fn submit_connect(
        &self,
        _: Arc<VerifiedSplitStep2>,
        _: ChildNumber,
        _: usize,
        _: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        unreachable!("the unified route never sends a step 2")
    }
    async fn submit_node(
        &self,
        _: Arc<VerifiedSplitStep2>,
        _: ChildNumber,
        _: usize,
        _: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        unreachable!("the unified route never sends a step 2")
    }
    async fn submit_unified_connect(
        &self,
        verified: Arc<VerifiedUnifiedSweep>,
        target: ChildNumber,
        binding: usize,
        _: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        self.send("connect", &verified, target, binding)
    }
    async fn submit_unified_node(
        &self,
        verified: Arc<VerifiedUnifiedSweep>,
        target: ChildNumber,
        binding: usize,
        _: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        self.send("node", &verified, target, binding)
    }
}

fn node_config(server: &MockServer) -> BitcoindConfig {
    BitcoindConfig {
        addr: *server.address(),
        rpc_auth: BitcoindRpcAuth::UserPass("synthetic".into(), "fixture".into()),
    }
}
/// The mocked Knots node: best block `hash(2)` around `testmempoolaccept`
/// of `tx`, `allowed` or not.
async fn knots(server: &MockServer, tx: &Transaction, allowed: bool) {
    for id in [1, 3] {
        server
            .mock_async(|when, then| {
                when.method(POST).path("/").json_body(
                    json!({"jsonrpc":"2.0","id":id,"method":"getbestblockhash","params":[]}),
                );
                then.status(200)
                    .json_body(json!({"id":id,"result":hash(2),"error":null}));
            })
            .await;
    }
    let hex = coincube_core::miniscript::bitcoin::consensus::encode::serialize_hex(tx);
    let mut row = json!({"txid":tx.compute_txid(),"wtxid":tx.compute_wtxid(),"allowed":allowed});
    if !allowed {
        row["reject-reason"] = json!("node-policy");
    }
    server
        .mock_async(|when, then| {
            when.method(POST).path("/").json_body(
                json!({"jsonrpc":"2.0","id":2,"method":"testmempoolaccept","params":[[hex]]}),
            );
            then.status(200)
                .json_body(json!({"id":2,"result":[row],"error":null}));
        })
        .await;
}

/// A unified coordinator over the view, on the node route (`node`) or the
/// Connect route.
struct Flow {
    temp: Temp,
    sender: watch::Sender<u64>,
    chains: ForkChains,
    vault: UnifiedVault,
    connect: MockServer,
    node: MockServer,
    use_node: bool,
    wallet: UnifiedWallet,
    coordinator: UnifiedCoordinator,
}
impl Flow {
    async fn new(use_node: bool) -> Self {
        Self::open(use_node, Temp::new(), false).await.unwrap()
    }
    async fn open(use_node: bool, temp: Temp, resume: bool) -> Result<Self, Error> {
        let wallet = unified_wallet();
        let coins = unified_coins(&wallet);
        let sender = watch::channel(7).0;
        let chains = ForkChains::new(coins.iter().map(|c| c.outpoint).collect());
        let connect = MockServer::start_async().await;
        let node = MockServer::start_async().await;
        let configured = use_node.then(|| node_config(&node));
        let daemon = UnifiedVault::new(configured.clone(), temp.0.clone());
        let routes = Step2Routes::for_test(
            daemon.clone(),
            PreflightClient::new(
                &connect.base_url(),
                CollectionContext {
                    expected_generation: 7,
                    generation: sender.subscribe(),
                },
            )
            .unwrap(),
            ORIGIN.to_owned(),
            vault(),
            configured,
            7,
            sender.subscribe(),
        );
        let coordinator = UnifiedCoordinator::open(
            &temp.0,
            TARGET.into(),
            wallet.source.clone(),
            coins,
            FORK,
            context(),
            sender.subscribe(),
            Box::new(chains.clone()),
            Box::new(routes),
            policy(),
            resume,
        )?;
        Ok(Self {
            temp,
            sender,
            chains,
            vault: daemon,
            connect,
            node,
            use_node,
            wallet,
            coordinator,
        })
    }
    fn target(&self) -> String {
        address(&vault(), INDEX).to_string()
    }
    async fn reserve(&mut self, index: u32) -> Result<u32, TargetError> {
        let polls = Arc::new(AtomicUsize::new(0));
        self.coordinator
            .reserve_target(
                &context(),
                reserved(&vault(), index, &polls),
                Duration::from_secs(5),
            )
            .await
    }
    /// Reserved, proven, built and signed; the route's preflight answers
    /// `allowed` for the verified sweep, which is returned.
    async fn signed(&mut self, allowed: bool) -> Transaction {
        assert_eq!(self.reserve(INDEX).await.unwrap(), INDEX);
        self.coordinator.prove_target(&context()).await.unwrap();
        let psbt = self
            .coordinator
            .build(&context(), &Fees(Some(2)))
            .await
            .unwrap();
        self.sign(&psbt, allowed).await
    }
    async fn sign(&mut self, psbt: &Psbt, allowed: bool) -> Transaction {
        let signed = sign_unified(&self.wallet, psbt);
        assert_eq!(
            self.coordinator.verify_signed(&context(), &signed).unwrap(),
            UnifiedReplayStatus::Protected
        );
        let tx = self.coordinator.transaction().unwrap().clone();
        if self.use_node {
            knots(&self.node, &tx, allowed).await;
        } else {
            mock_preflight(&self.connect, &tx, allowed).await;
        }
        tx
    }
    fn journal_exists(&self) -> bool {
        self.temp.0.join("intent.json").exists()
    }
}

/// U1: the fork-only observation path reads the BTCB2 anchor, the sweep
/// twice and the anchor again, and nothing on Bitcoin (every Bitcoin
/// observation read panics). A tip that moves inside the collection is
/// `Changed`; a stale anchor is `Stale`; the Bitcoin chain is refused before
/// any read. A sighting carries its block, checked at its height.
#[tokio::test(flavor = "multi_thread")]
async fn fork_only_observation_reads_only_the_fork_chain() {
    let chains = ForkChains::new(BTreeSet::new());
    let (_sender, generation) = watch::channel(7u64);
    let txid = Txid::from_byte_array([9; 32]);
    let collect = |chain| {
        let generation = generation.clone();
        let chains = chains.clone();
        async move {
            collect_fork_sweep(
                &chains,
                chain,
                txid,
                policy().observations,
                Duration::from_secs(2),
                CollectionContext {
                    expected_generation: 7,
                    generation,
                },
            )
            .await
        }
    };
    let view = collect(ChainId::BitcoinBlake2b).await.unwrap();
    assert_eq!(view.transaction(), TransactionObservation::Absent);
    assert_eq!(
        view.anchor().tip,
        BlockRef {
            height: BTCB2_TIP,
            hash: hash(2)
        }
    );
    assert_eq!(view.anchor().fork_height, FORK);
    assert_eq!(chains.0.lock().unwrap().anchor_reads, 2);
    // Confirmed in a block of the best chain.
    let block = BlockRef {
        height: BTCB2_TIP - 1,
        hash: hash(0x66),
    };
    chains.edit(|v| v.seen = vec![(txid, TransactionObservation::Confirmed { txid, block })]);
    assert_eq!(
        collect(ChainId::BitcoinBlake2b)
            .await
            .unwrap()
            .transaction(),
        TransactionObservation::Confirmed { txid, block }
    );
    chains.edit(|v| v.seen.clear());
    // The tip moves between the two anchor reads.
    chains.edit(|v| {
        v.inside = Some(Box::new(|v: &mut Fork| {
            v.tip = BlockRef {
                height: BTCB2_TIP + 1,
                hash: hash(3),
            }
        }))
    });
    assert_eq!(
        collect(ChainId::BitcoinBlake2b).await.err(),
        Some(Failure {
            stage: Stage::ForkTransaction,
            kind: FailureKind::Changed
        })
    );
    // A stale anchor.
    chains.edit(|v| v.anchor_age = 3_600);
    assert_eq!(
        collect(ChainId::BitcoinBlake2b).await.err(),
        Some(Failure {
            stage: Stage::ForkAnchor,
            kind: FailureKind::Stale
        })
    );
    // Not a fork chain: refused before any read.
    let reads = chains.0.lock().unwrap().anchor_reads;
    assert_eq!(
        collect(ChainId::Bitcoin).await.err(),
        Some(Failure {
            stage: Stage::Plan,
            kind: FailureKind::WrongChain
        })
    );
    assert_eq!(chains.0.lock().unwrap().anchor_reads, reads);
}

/// The C2 gate: every review refusal, each from a fully signed sweep, leaves
/// no journal and sends nothing; a refused preflight keeps the verified
/// sweep. Build refuses with no fee (D4) and with an anchor naming another
/// fork height (D10).
#[tokio::test(flavor = "multi_thread")]
async fn unified_review_requires_c2_evidence() {
    // Build-time refusals.
    let mut f = Flow::new(false).await;
    f.reserve(INDEX).await.unwrap();
    assert!(matches!(
        f.coordinator.build(&context(), &Fees(Some(2))).await,
        Err(UnifiedError::TargetNotProven)
    ));
    f.coordinator.prove_target(&context()).await.unwrap();
    assert!(matches!(
        f.coordinator.build(&context(), &Fees(None)).await,
        Err(UnifiedError::FeeUnavailable)
    ));
    f.chains.edit(|v| v.fork_height = FORK + 1);
    assert!(matches!(
        f.coordinator.build(&context(), &Fees(Some(2))).await,
        Err(UnifiedError::ForkHeightChanged {
            authenticated: FORK,
            anchor
        }) if anchor == FORK + 1
    ));
    assert!(matches!(
        f.coordinator.verify_signed(
            &context(),
            &sign_unified(
                &f.wallet,
                unified_sweep(&f.wallet, &address(&vault(), INDEX).script_pubkey(), 1_000).psbt()
            )
        ),
        Err(UnifiedError::NotBuilt)
    ));

    type Setup = Box<dyn Fn(&Flow, &Transaction)>;
    type Expected = fn(&UnifiedError) -> bool;
    let cases: Vec<(&str, Setup, Expected)> = vec![
        (
            "stale anchor",
            Box::new(|f, _| f.chains.edit(|v| v.anchor_age = 3_600)),
            |e| {
                matches!(
                    e,
                    UnifiedError::Coordinator(Error::Observation(Failure {
                        stage: Stage::ForkAnchor,
                        kind: FailureKind::Stale
                    }))
                )
            },
        ),
        (
            "anchor naming another fork height",
            Box::new(|f, _| f.chains.edit(|v| v.fork_height = FORK + 1)),
            |e| matches!(e, UnifiedError::ForkHeightChanged { .. }),
        ),
        (
            "sweep already on BTCB2",
            Box::new(|f, tx| {
                let txid = tx.compute_txid();
                f.chains
                    .edit(|v| v.seen = vec![(txid, TransactionObservation::Unconfirmed { txid })])
            }),
            |e| matches!(e, UnifiedError::SweepSeen(_)),
        ),
        (
            "coin spent on BTCB2",
            Box::new(|f, _| {
                f.chains.edit(|v| {
                    let first = *v.unspent.iter().next().unwrap();
                    v.unspent.remove(&first);
                })
            }),
            |e| matches!(e, UnifiedError::CoinSpent(_)),
        ),
        (
            "target used on BTCB2",
            Box::new(|f, _| {
                let target = f.target();
                f.chains
                    .edit(|v| v.used = vec![(ChainId::BitcoinBlake2b, target)])
            }),
            |e| {
                matches!(
                    e,
                    UnifiedError::Target(TargetError::Used(ChainId::BitcoinBlake2b))
                )
            },
        ),
        (
            "target used on Bitcoin",
            Box::new(|f, _| {
                let target = f.target();
                f.chains.edit(|v| v.used = vec![(ChainId::Bitcoin, target)])
            }),
            |e| matches!(e, UnifiedError::Target(TargetError::Used(ChainId::Bitcoin))),
        ),
        (
            "view changed between the collections",
            Box::new(|f, _| {
                f.chains.edit(|v| {
                    v.between = Some(Box::new(|v: &mut Fork| {
                        v.tip = BlockRef {
                            height: BTCB2_TIP + 1,
                            hash: hash(3),
                        }
                    }))
                })
            }),
            |e| matches!(e, UnifiedError::Coordinator(Error::ChangedReview)),
        ),
        (
            "revoked",
            Box::new(|f, _| f.coordinator.revoker().revoke()),
            |e| matches!(e, UnifiedError::Coordinator(Error::Revoked)),
        ),
    ];
    for (name, setup, expected) in cases {
        let mut f = Flow::new(false).await;
        let tx = f.signed(true).await;
        setup(&f, &tx);
        let result = f.coordinator.prepare_review(&context()).await;
        assert!(
            result.as_ref().err().is_some_and(expected),
            "{}: {:?}",
            name,
            result.err()
        );
        assert!(!f.journal_exists(), "{}", name);
        assert!(f.vault.sends().is_empty(), "{}", name);
    }
    // A refused preflight, on either route: nothing recorded, the verified
    // sweep kept.
    for use_node in [false, true] {
        let mut f = Flow::new(use_node).await;
        let tx = f.signed(false).await;
        assert!(matches!(
            f.coordinator.prepare_review(&context()).await,
            Err(UnifiedError::Coordinator(Error::PolicyRejected(_)))
        ));
        assert_eq!(f.coordinator.transaction(), Some(&tx));
        assert!(!f.journal_exists());
        assert!(f.vault.sends().is_empty());
    }
    // A generation change revokes too.
    let mut f = Flow::new(false).await;
    f.signed(true).await;
    f.sender.send(8).unwrap();
    assert!(matches!(
        f.coordinator.prepare_review(&context()).await,
        Err(UnifiedError::Coordinator(Error::Revoked))
    ));
}

/// U3, U4: on either route the review creates nothing; confirmation checks
/// it again, creates the fork-only journal, records the intent with the
/// verified sweep and only then sends it, once, with the reserved index. A
/// recorded submission is never reviewed or sent again. A backend switch
/// after the review refuses before the journal exists; a send the daemon
/// refuses after the intent is `Uncertain` and never resent.
#[tokio::test(flavor = "multi_thread")]
async fn unified_confirm_creates_journal_records_intent_then_sends_once() {
    for use_node in [false, true] {
        let mut f = Flow::new(use_node).await;
        let tx = f.signed(true).await;
        let review = f.coordinator.prepare_review(&context()).await.unwrap();
        let snapshot = review.snapshot();
        assert_eq!(snapshot.txid, tx.compute_txid());
        assert_eq!(snapshot.target_index, INDEX);
        assert_eq!(snapshot.replay, UnifiedReplayStatus::Protected);
        assert_eq!(snapshot.route, f.coordinator.route());
        assert_eq!(
            snapshot.route.label(),
            if use_node {
                "Your Bitcoin node"
            } else {
                "Connect"
            }
        );
        assert!(!f.journal_exists(), "U3: no journal before confirmation");
        let outcome = f
            .coordinator
            .confirm_and_submit(review, &context())
            .await
            .unwrap();
        assert_eq!(
            outcome,
            Outcome::UpstreamAccepted {
                txid: tx.compute_txid(),
                wtxid: tx.compute_wtxid(),
            }
        );
        let route = if use_node { "node" } else { "connect" };
        assert_eq!(
            f.vault.sends(),
            vec![(
                route,
                tx.compute_txid(),
                ChildNumber::from_normal_idx(INDEX).unwrap(),
                true
            )],
            "sent once, after the intent was recorded"
        );
        let journal = f.temp.journal();
        assert_eq!(journal["version"], 9);
        assert_eq!(journal["split"]["kind"], "Unified");
        assert_eq!(journal["split"]["target_index"], INDEX);
        assert_eq!(
            journal["fork_submission"]["txid"],
            tx.compute_txid().to_string()
        );
        assert_eq!(
            journal["split"]["step2_transaction"],
            serde_json::to_value(&tx).unwrap()
        );
        assert_eq!(
            f.coordinator.recorded_outcome(),
            Some(Outcome::Uncertain {
                txid: tx.compute_txid(),
                wtxid: tx.compute_wtxid(),
            })
        );
        assert!(matches!(
            f.coordinator.prepare_review(&context()).await,
            Err(UnifiedError::Coordinator(Error::SubmissionAlreadyRecorded))
        ));
        // The coordinator reconciles; it never sends again.
        assert_eq!(
            f.coordinator
                .reconcile_sweep(&context())
                .await
                .unwrap()
                .sweep,
            TransactionObservation::Absent
        );
        assert_eq!(f.vault.sends().len(), 1);
    }
    // A backend switch after the review: refused before the journal exists.
    for use_node in [false, true] {
        let mut f = Flow::new(use_node).await;
        f.signed(true).await;
        let review = f.coordinator.prepare_review(&context()).await.unwrap();
        f.vault.binding.store(2, Ordering::SeqCst);
        assert!(matches!(
            f.coordinator.confirm_and_submit(review, &context()).await,
            Err(UnifiedError::Coordinator(Error::Preflight(
                claim_preflight::Error::BackendChanged
            )))
        ));
        assert!(!f.journal_exists());
        assert!(f.vault.sends().is_empty());
    }
    // The daemon refuses the send after the intent: Uncertain, never resent.
    let mut f = Flow::new(false).await;
    let tx = f.signed(true).await;
    f.vault.accept.store(false, Ordering::SeqCst);
    let review = f.coordinator.prepare_review(&context()).await.unwrap();
    assert_eq!(
        f.coordinator
            .confirm_and_submit(review, &context())
            .await
            .unwrap(),
        Outcome::Uncertain {
            txid: tx.compute_txid(),
            wtxid: tx.compute_wtxid(),
        }
    );
    assert_eq!(f.vault.sends().len(), 1);
    assert!(f.temp.journal().get("fork_submission").is_some());
    assert!(matches!(
        f.coordinator.prepare_review(&context()).await,
        Err(UnifiedError::Coordinator(Error::SubmissionAlreadyRecorded))
    ));
    assert_eq!(f.vault.sends().len(), 1);
}

/// The target is the transport Vault's own receive derivation, reserved
/// once; one proven used on either chain may be replaced, by a strictly
/// higher index only, and a new reservation needs a new proof and build.
#[tokio::test(flavor = "multi_thread")]
async fn unified_target_is_the_vaults_and_replaced_only_when_used() {
    let mut f = Flow::new(false).await;
    let polls = Arc::new(AtomicUsize::new(0));
    assert!(matches!(
        f.coordinator
            .reserve_target(
                &context(),
                reserved(&other_vault(), INDEX, &polls),
                Duration::from_secs(5)
            )
            .await,
        Err(TargetError::NotTargetVault)
    ));
    assert!(matches!(
        f.coordinator.prove_target(&context()).await,
        Err(TargetError::NoReservation)
    ));
    assert_eq!(f.reserve(INDEX).await.unwrap(), INDEX);
    assert!(matches!(
        f.reserve(INDEX + 1).await,
        Err(TargetError::AlreadyReserved)
    ));
    let target = f.target();
    f.chains.edit(|v| v.used = vec![(ChainId::Bitcoin, target)]);
    assert!(matches!(
        f.coordinator.prove_target(&context()).await,
        Err(TargetError::Used(ChainId::Bitcoin))
    ));
    assert!(matches!(
        f.coordinator.build(&context(), &Fees(Some(2))).await,
        Err(UnifiedError::TargetNotProven)
    ));
    assert!(matches!(
        f.reserve(INDEX).await,
        Err(TargetError::ReservationUnavailable)
    ));
    assert_eq!(f.reserve(INDEX + 1).await.unwrap(), INDEX + 1);
    assert_eq!(f.coordinator.target_index(), Some(INDEX + 1));
    f.coordinator.prove_target(&context()).await.unwrap();
    let psbt = f
        .coordinator
        .build(&context(), &Fees(Some(2)))
        .await
        .unwrap();
    assert_eq!(
        psbt.unsigned_tx.output[0].script_pubkey,
        address(&vault(), INDEX + 1).script_pubkey()
    );
}

/// Restart of an unsubmitted fork-only record (a confirmation that created
/// the journal and stopped before its intent): the recorded target is kept
/// and cannot be replaced, the recorded sweep is rebuilt exactly with no fee
/// and revalidated, and the reviewed sweep is then recorded and sent once. A
/// record with another fork height, or one with a recorded submission, is
/// refused; other coins cannot rebuild it.
#[tokio::test(flavor = "multi_thread")]
async fn unified_restart_revalidates_an_unsubmitted_record() {
    let wallet = unified_wallet();
    let target = address(&vault(), INDEX).script_pubkey();
    let sweep = unified_sweep(&wallet, &target, BTCB2_TIP as u32);
    let temp = Temp::new();
    drop(
        Controller::create_unified_split(&temp.0, TARGET.into(), &sweep, INDEX, context()).unwrap(),
    );
    let journal = temp.journal();
    let mut f = Flow::open(false, temp, true).await.unwrap();
    assert_eq!(f.coordinator.target_index(), Some(INDEX));
    assert!(matches!(
        f.reserve(INDEX + 1).await,
        Err(TargetError::AlreadyReserved)
    ));
    f.coordinator.prove_target(&context()).await.unwrap();
    // Recorded: rebuilt without a fee.
    let psbt = f.coordinator.build(&context(), &Fees(None)).await.unwrap();
    assert_eq!(&psbt, sweep.psbt());
    let tx = f.sign(&psbt, true).await;
    assert_eq!(
        f.temp.journal(),
        journal,
        "nothing written before confirmation"
    );
    let review = f.coordinator.prepare_review(&context()).await.unwrap();
    f.coordinator
        .confirm_and_submit(review, &context())
        .await
        .unwrap();
    assert_eq!(f.vault.sends().len(), 1);
    assert!(f.vault.sends()[0].3);
    assert_eq!(
        f.temp.journal()["fork_submission"]["txid"],
        tx.compute_txid().to_string()
    );
    let Flow {
        temp, coordinator, ..
    } = f;
    drop(coordinator);
    // A recorded submission: not this coordinator's.
    assert!(matches!(
        Flow::open(false, temp, true).await.err(),
        Some(Error::SubmissionAlreadyRecorded)
    ));

    // Another fork height than the record's.
    let temp = Temp::new();
    drop(
        Controller::create_unified_split(&temp.0, TARGET.into(), &sweep, INDEX, context()).unwrap(),
    );
    let wallet = unified_wallet();
    let sender = watch::channel(7).0;
    let chains = ForkChains::new(BTreeSet::new());
    let routes = |vault: UnifiedVault| {
        Step2Routes::for_test(
            vault,
            PreflightClient::new(
                "http://127.0.0.1:9",
                CollectionContext {
                    expected_generation: 7,
                    generation: sender.subscribe(),
                },
            )
            .unwrap(),
            ORIGIN.to_owned(),
            super::vault(),
            None,
            7,
            sender.subscribe(),
        )
    };
    assert!(matches!(
        UnifiedCoordinator::open(
            &temp.0,
            TARGET.into(),
            wallet.source.clone(),
            unified_coins(&wallet),
            FORK + 1,
            context(),
            sender.subscribe(),
            Box::new(chains.clone()),
            Box::new(routes(UnifiedVault::new(None, temp.0.clone()))),
            policy(),
            true,
        )
        .err(),
        Some(Error::InvalidBinding)
    ));
    // Other coins cannot rebuild the recorded sweep.
    let mut fewer = UnifiedCoordinator::open(
        &temp.0,
        TARGET.into(),
        wallet.source.clone(),
        unified_coins(&wallet)[..1].to_vec(),
        FORK,
        context(),
        sender.subscribe(),
        Box::new(chains.clone()),
        Box::new(routes(UnifiedVault::new(None, temp.0.clone()))),
        policy(),
        true,
    )
    .unwrap();
    fewer.prove_target(&context()).await.unwrap();
    assert!(matches!(
        fewer.build(&context(), &Fees(None)).await,
        Err(UnifiedError::Construction(_))
    ));
}

/// U2: the fork-only reconciler reopens only a fork-only record with a
/// recorded submission. It holds no transport and sends nothing; each
/// reconcile observes the recorded sweep on BTCB2 only, and a sighting (in a
/// mempool or a block, or by a read of a collection that then failed) is
/// recorded and survives reopen. Another session revokes it.
#[tokio::test(flavor = "multi_thread")]
async fn unified_reconciler_records_sightings_and_sends_nothing() {
    let (sender, _) = watch::channel(7);
    let chains = ForkChains::new(BTreeSet::new());
    let open = |temp: &Temp, digest| {
        UnifiedReconciler::open(
            &temp.0,
            TARGET.into(),
            digest,
            context(),
            sender.subscribe(),
            Box::new(chains.clone()),
            policy(),
        )
    };
    // Not a fork-only record with a submission: refused.
    let (unsubmitted, digest, _) = fork_only_journal(false);
    assert!(matches!(
        open(&unsubmitted, digest),
        Err(Error::InvalidBinding)
    ));
    let two_step = Harness::new(6).await;
    assert!(matches!(
        open(&two_step.temp, two_step.step1.source().digest()),
        Err(Error::InvalidBinding)
    ));

    let (temp, digest, signed) = fork_only_journal(true);
    let txid = signed.compute_txid();
    let mut reconciler = open(&temp, digest).unwrap();
    assert_eq!(
        reconciler.recorded_outcome(),
        Some(Outcome::Uncertain {
            txid,
            wtxid: signed.compute_wtxid(),
        })
    );
    let observed = |temp: &Temp| temp.journal()["split"]["step2_observed"] == true;
    let reconciled = reconciler.reconcile_sweep(&context()).await.unwrap();
    assert_eq!(reconciled.sweep, TransactionObservation::Absent);
    assert_eq!(reconciled.fork_tip.height, BTCB2_TIP);
    assert!(!observed(&temp));
    // Seen in the mempool: recorded.
    chains.edit(|v| v.seen = vec![(txid, TransactionObservation::Unconfirmed { txid })]);
    assert_eq!(
        reconciler.reconcile_sweep(&context()).await.unwrap().sweep,
        TransactionObservation::Unconfirmed { txid }
    );
    assert!(observed(&temp));
    // Another session revokes it.
    let mut other = context();
    other.account = "other".into();
    assert!(matches!(
        reconciler.reconcile_sweep(&other).await,
        Err(Error::Revoked)
    ));
    drop(reconciler);
    let controller = Controller::reopen(
        &temp.0,
        &claim_workflow::split_identity(TARGET.into(), digest),
        context(),
    )
    .unwrap();
    assert!(controller.split_step2_observed());
    drop(controller);

    // A sighting by a read of a collection that then failed (the next read
    // says absent: `Changed`) is recorded all the same.
    let (temp, digest, signed) = fork_only_journal(true);
    let txid = signed.compute_txid();
    chains.edit(|v| {
        v.seen.clear();
        v.seen_once = Some((txid, TransactionObservation::Unconfirmed { txid }));
    });
    let mut reconciler = open(&temp, digest).unwrap();
    assert!(matches!(
        reconciler.reconcile_sweep(&context()).await,
        Err(Error::Observation(Failure {
            kind: FailureKind::Changed,
            ..
        }))
    ));
    assert!(observed(&temp));
}
