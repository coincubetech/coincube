//! Split (#568 B3b-2) step-2 routes end to end through the coordinator, over
//! a substitute target-Vault daemon whose backend binding a test can switch:
//! the P4 node route against a mocked Knots RPC node (best block, then
//! `testmempoolaccept`), the Connect route's binding at review, and restart
//! after a recorded submission, which can only reconcile.
use super::*;
use crate::services::claim_coordinator::fork::split::step2::{
    SplitStep2Reconciler, Step2Daemon, Step2Routes,
};
use coincubed::config::{BitcoindConfig, BitcoindRpcAuth};

/// One recorded send: route, txid, target index, binding.
type Sent = (&'static str, Txid, ChildNumber, usize);

/// A target Vault daemon whose backend binding is a number a test can change
/// (a backend switch, daemon restart or node change), and whose node is
/// configurable. Its submission refuses a binding other than the current one,
/// as the real daemon does.
#[derive(Clone)]
struct Vault {
    binding: Arc<AtomicUsize>,
    node: Arc<Mutex<Option<BitcoindConfig>>>,
    sends: Arc<Mutex<Vec<Sent>>>,
}
impl Vault {
    fn new(node: Option<BitcoindConfig>) -> Self {
        Self {
            binding: Arc::new(AtomicUsize::new(1)),
            node: Arc::new(Mutex::new(node)),
            sends: Arc::new(Mutex::new(Vec::new())),
        }
    }
    fn send(
        &self,
        route: &'static str,
        verified: &VerifiedSplitStep2,
        target: ChildNumber,
        binding: usize,
    ) -> Result<SubmissionOutcome, DaemonError> {
        if binding != self.binding.load(Ordering::SeqCst) {
            return Err(DaemonError::PoisonSubmission(
                coincubed::poison_broadcast::SubmissionError::BackendUnavailable,
            ));
        }
        let tx = verified.transaction();
        self.sends
            .lock()
            .unwrap()
            .push((route, tx.compute_txid(), target, binding));
        Ok(SubmissionOutcome::UpstreamAccepted {
            txid: tx.compute_txid(),
            wtxid: tx.compute_wtxid(),
        })
    }
}
#[async_trait]
impl Step2Daemon for Vault {
    type Binding = usize;
    async fn binding(&self) -> Result<usize, DaemonError> {
        Ok(self.binding.load(Ordering::SeqCst))
    }
    fn node(&self) -> Option<BitcoindConfig> {
        self.node.lock().unwrap().clone()
    }
    async fn submit_connect(
        &self,
        verified: Arc<VerifiedSplitStep2>,
        target: ChildNumber,
        binding: usize,
        _gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        self.send("connect", &verified, target, binding)
    }
    async fn submit_node(
        &self,
        verified: Arc<VerifiedSplitStep2>,
        target: ChildNumber,
        binding: usize,
        _gate: Arc<SubmissionGate>,
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
/// The mocked Knots node: best block `tip` around `testmempoolaccept` of
/// `tx`, `allowed` or not. Returns the acceptance mock's id.
async fn knots(server: &MockServer, tx: &Transaction, tip: BlockHash, allowed: bool) -> usize {
    for id in [1, 3] {
        server
            .mock_async(|when, then| {
                when.method(POST).path("/").json_body(
                    json!({"jsonrpc":"2.0","id":id,"method":"getbestblockhash","params":[]}),
                );
                then.status(200)
                    .json_body(json!({"id":id,"result":tip,"error":null}));
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
        .await
        .id
}
/// Step 2 checked, built, signed and handed to a coordinator over `vault`
/// on the node route (`node`) or the Connect route (`None`). The Connect
/// preflight answers `allowed` at `connect`; its mock's id comes back last.
async fn routed(
    vault: &Vault,
    node: Option<BitcoindConfig>,
    connect: &MockServer,
) -> (Harness, SplitStep2Coordinator, Transaction, usize) {
    let s = Step2::new().await;
    let signed_tx = s.signed_tx();
    let preflight = mock_preflight(connect, &signed_tx, true).await;
    let routes = Step2Routes::for_test(
        vault.clone(),
        PreflightClient::new(
            &connect.base_url(),
            CollectionContext {
                expected_generation: 7,
                generation: s.h.sender.subscribe(),
            },
        )
        .unwrap(),
        ORIGIN.to_owned(),
        vault_descriptor(),
        node,
        7,
        s.h.sender.subscribe(),
    );
    let signed = s.signed();
    let coins = coins(&s.h.wallet);
    let coordinator = s
        .preparation
        .finish_with(&context(), &signed, &coins, Box::new(routes))
        .unwrap();
    (s.h, coordinator, signed_tx, preflight)
}
fn vault_descriptor() -> CoincubeDescriptor {
    super::vault()
}

/// P4 end to end: the node route preflights only on the bound Knots node, at
/// the Connect-observed BTCB2 tip, labels the review with the node, records
/// the intent and submits once to the node with the binding captured at
/// review. Connect's preflight is never asked.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_node_route_reviews_on_the_node_and_submits_there_once() {
    // One pooled httpmock server is both the node (JSON-RPC at `/`) and
    // Connect (its Esplora paths): a test never waits for a second server
    // while holding one (the pool deadlock under load; see `BitcoinPreflight`).
    let node = MockServer::start_async().await;
    let connect = &node;
    let vault = Vault::new(Some(node_config(&node)));
    let (h, mut coordinator, signed_tx, preflight) =
        routed(&vault, Some(node_config(&node)), connect).await;
    knots(&node, &signed_tx, hash(2), true).await;
    let review = coordinator.prepare_review(&context()).await.unwrap();
    let route = review.snapshot().route;
    assert!(
        matches!(route, SubmissionRoute::BitcoinNode { address, .. } if address == *node.address())
    );
    assert_eq!(route.label(), "Your Bitcoin node");
    let outcome = coordinator
        .confirm_and_submit(review, &context())
        .await
        .unwrap();
    assert_eq!(
        outcome,
        Outcome::UpstreamAccepted {
            txid: signed_tx.compute_txid(),
            wtxid: signed_tx.compute_wtxid(),
        }
    );
    assert_eq!(
        *vault.sends.lock().unwrap(),
        vec![(
            "node",
            signed_tx.compute_txid(),
            ChildNumber::from_normal_idx(INDEX).unwrap(),
            1
        )]
    );
    // Connect's BTCB2 preflight was registered but never asked (#659 N2).
    let journal = h.temp.journal();
    assert_eq!(
        journal["fork_submission"]["txid"],
        signed_tx.compute_txid().to_string()
    );
    assert_eq!(
        httpmock::Mock::new(preflight, connect).hits_async().await,
        0,
        "Connect's preflight is never asked on the node route"
    );
}

/// P4: a node on another chain (its best block is not the BTCB2 tip) or one
/// that refuses the transaction blocks review, records nothing and falls
/// back to nothing; the signed step 2 is kept.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_node_route_refuses_another_chain_or_a_node_rejection() {
    for (tip, allowed) in [(hash(0x77), true), (hash(2), false)] {
        // One pooled httpmock server is both the node (JSON-RPC at `/`) and
        // Connect (its Esplora paths): a test never waits for a second server
        // while holding one (the pool deadlock under load; see `BitcoinPreflight`).
        let node = MockServer::start_async().await;
        let connect = &node;
        let vault = Vault::new(Some(node_config(&node)));
        let (h, mut coordinator, signed_tx, _) =
            routed(&vault, Some(node_config(&node)), connect).await;
        knots(&node, &signed_tx, tip, allowed).await;
        let result = coordinator.prepare_review(&context()).await;
        if allowed {
            assert!(
                matches!(result, Err(Error::Preflight(claim_preflight::Error::Stale))),
                "{:?}",
                result.err()
            );
        } else {
            assert!(matches!(
                result,
                Err(Error::PolicyRejected(NodePolicy::Rejected { .. }))
            ));
        }
        assert_eq!(coordinator.transaction(), &signed_tx);
        assert!(h.temp.journal().get("fork_submission").is_none());
        assert!(vault.sends.lock().unwrap().is_empty());
    }
}

/// #630 F3 and the backend-switch carry-forward: on either route the daemon
/// binding seen at the first review must hold at every later review. A
/// switch between review and confirmation (a daemon restart, a backend
/// switch, a node change) refuses as `BackendChanged` before any intent is
/// recorded; nothing is sent.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_backend_switch_after_review_refuses_before_any_intent() {
    for (case, use_node) in [
        ("connect binding", false),
        ("node binding", true),
        ("node config", true),
        ("connect becomes node", false),
    ] {
        // One pooled httpmock server is both the node (JSON-RPC at `/`) and
        // Connect (its Esplora paths): a test never waits for a second server
        // while holding one (the pool deadlock under load; see `BitcoinPreflight`).
        let node = MockServer::start_async().await;
        let connect = &node;
        let configured = use_node.then(|| node_config(&node));
        let vault = Vault::new(configured.clone());
        let (h, mut coordinator, signed_tx, _) = routed(&vault, configured, connect).await;
        knots(&node, &signed_tx, hash(2), true).await;
        let review = coordinator.prepare_review(&context()).await.unwrap();
        match case {
            "connect binding" | "node binding" => {
                vault.binding.store(2, Ordering::SeqCst);
            }
            "node config" => {
                let mut other = node_config(&node);
                other.rpc_auth = BitcoindRpcAuth::UserPass("other".into(), "fixture".into());
                *vault.node.lock().unwrap() = Some(other);
            }
            _ => *vault.node.lock().unwrap() = Some(node_config(&node)),
        }
        assert!(
            matches!(
                coordinator.confirm_and_submit(review, &context()).await,
                Err(Error::Preflight(claim_preflight::Error::BackendChanged))
            ),
            "{}",
            case
        );
        assert!(
            h.temp.journal().get("fork_submission").is_none(),
            "{}",
            case
        );
        assert!(vault.sends.lock().unwrap().is_empty(), "{}", case);
        // A later review on the switched backend refuses the same way.
        assert!(matches!(
            coordinator.prepare_review(&context()).await,
            Err(Error::Preflight(claim_preflight::Error::BackendChanged))
        ));
    }
}

/// A switch in the instant between the last review and the send is refused
/// by the daemon (it checks the captured binding); the intent was already
/// recorded, so the outcome is Uncertain and only reconciles.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_switch_during_send_is_refused_by_the_daemon_and_uncertain() {
    let connect = MockServer::start_async().await;
    let vault = Vault::new(None);
    // A daemon whose binding moves when it is asked to send.
    #[derive(Clone)]
    struct Racing(Vault);
    #[async_trait]
    impl Step2Daemon for Racing {
        type Binding = usize;
        async fn binding(&self) -> Result<usize, DaemonError> {
            self.0.binding().await
        }
        fn node(&self) -> Option<BitcoindConfig> {
            None
        }
        async fn submit_connect(
            &self,
            verified: Arc<VerifiedSplitStep2>,
            target: ChildNumber,
            binding: usize,
            gate: Arc<SubmissionGate>,
        ) -> Result<SubmissionOutcome, DaemonError> {
            self.0.binding.store(9, Ordering::SeqCst);
            self.0.submit_connect(verified, target, binding, gate).await
        }
        async fn submit_node(
            &self,
            _: Arc<VerifiedSplitStep2>,
            _: ChildNumber,
            _: usize,
            _: Arc<SubmissionGate>,
        ) -> Result<SubmissionOutcome, DaemonError> {
            unreachable!()
        }
    }
    let s = Step2::new().await;
    let signed_tx = s.signed_tx();
    mock_preflight(&connect, &signed_tx, true).await;
    let routes = Step2Routes::for_test(
        Racing(vault.clone()),
        PreflightClient::new(
            &connect.base_url(),
            CollectionContext {
                expected_generation: 7,
                generation: s.h.sender.subscribe(),
            },
        )
        .unwrap(),
        ORIGIN.to_owned(),
        vault_descriptor(),
        None,
        7,
        s.h.sender.subscribe(),
    );
    let signed = s.signed();
    let coins = coins(&s.h.wallet);
    let h = s.h;
    let mut coordinator = s
        .preparation
        .finish_with(&context(), &signed, &coins, Box::new(routes))
        .unwrap();
    let review = coordinator.prepare_review(&context()).await.unwrap();
    assert_eq!(
        coordinator
            .confirm_and_submit(review, &context())
            .await
            .unwrap(),
        Outcome::Uncertain {
            txid: signed_tx.compute_txid(),
            wtxid: signed_tx.compute_wtxid(),
        }
    );
    assert!(vault.sends.lock().unwrap().is_empty());
    assert!(h.temp.journal().get("fork_submission").is_some());
}

/// Restart after a recorded step-2 submission: the reconciler reopens the
/// journal with no construction, coins or transport, reports the recorded
/// signed txid as uncertain and reconciles it on BTCB2 (absent, then seen,
/// then confirmed) without ever sending. A preparation refuses the journal,
/// and a journal with no recorded step 2 is not a reconciler's.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_restart_after_submission_only_reconciles() {
    let connect = MockServer::start_async().await;
    let vault = Vault::new(None);
    let (h, mut coordinator, signed_tx, _) = routed(&vault, None, &connect).await;
    // A journal with no recorded step-2 submission is not a reconciler's.
    let unsubmitted = Harness::new(6).await;
    assert!(matches!(
        SplitStep2Reconciler::open(
            &unsubmitted.temp.0,
            TARGET.into(),
            unsubmitted.step1.source().digest(),
            context(),
            unsubmitted.sender.subscribe(),
            Box::new(unsubmitted.chains.clone()),
            policy(),
        ),
        Err(Error::InvalidBinding)
    ));
    let review = coordinator.prepare_review(&context()).await.unwrap();
    coordinator
        .confirm_and_submit(review, &context())
        .await
        .unwrap();
    // The coordinator itself reconciles after the send.
    let seen = coordinator.reconcile_sweep(&context()).await.unwrap().step2;
    assert_eq!(seen, TransactionObservation::Absent);
    drop(coordinator);

    assert!(matches!(h.prepare(), Err(Error::SubmissionAlreadyRecorded)));
    let mut reconciler = SplitStep2Reconciler::open(
        &h.temp.0,
        TARGET.into(),
        h.step1.source().digest(),
        context(),
        h.sender.subscribe(),
        Box::new(h.chains.clone()),
        policy(),
    )
    .unwrap();
    assert_eq!(
        reconciler.recorded_outcome(),
        Some(Outcome::Uncertain {
            txid: signed_tx.compute_txid(),
            wtxid: signed_tx.compute_wtxid(),
        })
    );
    let txid = signed_tx.compute_txid();
    for seen in [
        TransactionObservation::Absent,
        TransactionObservation::Unconfirmed { txid },
        TransactionObservation::Confirmed {
            txid,
            block: BlockRef {
                height: FORK_TIP,
                hash: hash(2),
            },
        },
    ] {
        h.chains.edit(|view| view.on_btcb2 = vec![(txid, seen)]);
        if seen == TransactionObservation::Absent {
            h.chains.edit(|view| view.on_btcb2.clear());
        }
        let observed = reconciler.reconcile_sweep(&context()).await.unwrap().step2;
        assert_eq!(observed, seen);
    }
    assert!(
        vault.sends.lock().unwrap().len() == 1,
        "sent once, before the restart"
    );
    // Another context revokes it.
    let mut other = context();
    other.account = "other".into();
    assert!(matches!(
        reconciler.reconcile_sweep(&other).await,
        Err(Error::Revoked)
    ));
}

mod resend;
