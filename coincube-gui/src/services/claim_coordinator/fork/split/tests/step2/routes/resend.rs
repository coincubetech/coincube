//! P3-3 (#635 carry-forward on #568): a reviewed resend of exactly the
//! recorded signed step 2 after an uncertain submission, live and after a
//! restart. The dead end is reproduced first: the intent is recorded, then
//! the target Vault daemon refuses before any byte leaves, the outcome is
//! `Uncertain`, the ordinary review refuses and reconcile only observes.
use super::*;
use crate::services::claim_coordinator::fork::split::step2::{
    ResendError, SplitStep2Coordinator, Step2ResubmissionReview,
};

/// A target Vault daemon that refuses sends before any byte leaves: the
/// next `refusals` sends, and, when `switch` is set, the next send after its
/// backend binding moves to that value (the #635 dead end: a backend switch
/// during the send, refused by the daemon's binding check).
#[derive(Clone)]
struct Refusing {
    vault: Vault,
    refusals: Arc<AtomicUsize>,
    switch: Arc<Mutex<Option<usize>>>,
    /// Sends never answer (a lost response).
    hang: Arc<std::sync::atomic::AtomicBool>,
    /// Every send the coordinator asked for, refused or not.
    calls: Arc<AtomicUsize>,
}
impl Refusing {
    fn new(node: Option<BitcoindConfig>, refusals: usize) -> Self {
        Self {
            vault: Vault::new(node),
            refusals: Arc::new(AtomicUsize::new(refusals)),
            switch: Arc::new(Mutex::new(None)),
            hang: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn sends(&self) -> Vec<Sent> {
        self.vault.sends.lock().unwrap().clone()
    }
    /// Whether this send is refused before any byte leaves.
    fn refuse(&self) -> bool {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(binding) = self.switch.lock().unwrap().take() {
            self.vault.binding.store(binding, Ordering::SeqCst);
        }
        self.refusals
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }
}
#[async_trait]
impl Step2Daemon for Refusing {
    type Binding = usize;
    async fn binding(&self) -> Result<usize, DaemonError> {
        self.vault.binding().await
    }
    fn node(&self) -> Option<BitcoindConfig> {
        self.vault.node()
    }
    async fn submit_connect(
        &self,
        verified: Arc<VerifiedSplitStep2>,
        target: ChildNumber,
        binding: usize,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        if self.refuse() {
            return Err(DaemonError::PoisonSubmission(
                coincubed::poison_broadcast::SubmissionError::BackendUnavailable,
            ));
        }
        if self.hang.load(Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        self.vault
            .submit_connect(verified, target, binding, gate)
            .await
    }
    async fn submit_node(
        &self,
        verified: Arc<VerifiedSplitStep2>,
        target: ChildNumber,
        binding: usize,
        gate: Arc<SubmissionGate>,
    ) -> Result<SubmissionOutcome, DaemonError> {
        if self.refuse() {
            return Err(DaemonError::PoisonSubmission(
                coincubed::poison_broadcast::SubmissionError::BackendUnavailable,
            ));
        }
        self.vault
            .submit_node(verified, target, binding, gate)
            .await
    }
}

fn routes_over(
    daemon: Refusing,
    node: Option<BitcoindConfig>,
    connect: &MockServer,
    origin: &str,
    descriptor: CoincubeDescriptor,
    h: &Harness,
) -> Box<dyn Step2Transport> {
    Box::new(Step2Routes::for_test(
        daemon,
        PreflightClient::new(
            &connect.base_url(),
            CollectionContext {
                expected_generation: 7,
                generation: h.sender.subscribe(),
            },
        )
        .unwrap(),
        origin.to_owned(),
        descriptor,
        node,
        7,
        h.sender.subscribe(),
    ))
}

/// The dead end, reproduced.
struct Uncertain {
    h: Harness,
    coordinator: SplitStep2Coordinator,
    signed: Transaction,
    fee: u64,
    /// The Connect preflight mock's id.
    preflight: usize,
}
/// Step 2 checked, built, signed, reviewed and confirmed over `daemon`; its
/// intent is recorded, then the daemon refuses before any byte leaves.
async fn uncertain(
    daemon: &Refusing,
    node: Option<BitcoindConfig>,
    connect: &MockServer,
) -> Uncertain {
    let s = Step2::new().await;
    let signed = s.signed_tx();
    let fee = s.preparation.step2.as_ref().unwrap().fee().to_sat();
    let preflight = mock_preflight(connect, &signed, true).await;
    let transport = routes_over(
        daemon.clone(),
        node,
        connect,
        ORIGIN,
        vault_descriptor(),
        &s.h,
    );
    let psbt = s.signed();
    let coins = coins(&s.h.wallet);
    let Step2 { h, preparation, .. } = s;
    let mut coordinator = preparation
        .finish_with(&context(), &psbt, &coins, transport)
        .unwrap();
    let review = coordinator.prepare_review(&context()).await.unwrap();
    assert_eq!(
        coordinator
            .confirm_and_submit(review, &context())
            .await
            .unwrap(),
        Outcome::Uncertain {
            txid: signed.compute_txid(),
            wtxid: signed.compute_wtxid(),
        }
    );
    assert!(daemon.sends().is_empty(), "nothing left");
    assert_eq!(
        h.temp.journal()["fork_submission"]["txid"],
        signed.compute_txid().to_string()
    );
    Uncertain {
        h,
        coordinator,
        signed,
        fee,
        preflight,
    }
}

/// Restart: step 1 rebuilt and its signed bytes verified, the claimed coins,
/// and a transport for the target Vault at the Split's origin.
async fn reopen(
    h: &Harness,
    coins: Vec<SplitCoin>,
    transport: Box<dyn Step2Transport>,
) -> Result<SplitStep2Coordinator, Error> {
    SplitStep2Coordinator::open_uncertain(
        &h.temp.0,
        TARGET.into(),
        &h.step1,
        h.verified(),
        FORK,
        coins,
        context(),
        h.sender.subscribe(),
        Box::new(h.chains.clone()),
        transport,
        policy(),
    )
    .await
}

fn resends(h: &Harness) -> serde_json::Value {
    h.temp
        .journal()
        .get("split")
        .and_then(|split| split.get("step2_resubmissions"))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}
fn attempts(signed: &Transaction, n: usize) -> serde_json::Value {
    json!(vec![
        json!({ "wtxid": signed.compute_wtxid().to_string() });
        n
    ])
}

/// The live dead end and its recovery. Ordinary review and reconcile never
/// resend; a distinct review, which records nothing, then a confirmation
/// that records the attempt before the one send of exactly the recorded
/// bytes. The route's exact acceptance is recorded as a sighting: it left,
/// and no resend is offered again.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_dead_end_is_resent_live_after_an_explicit_review() {
    let connect = MockServer::start_async().await;
    let daemon = Refusing::new(None, 1);
    let Uncertain {
        h,
        mut coordinator,
        signed,
        fee,
        ..
    } = uncertain(&daemon, None, &connect).await;
    let (txid, wtxid) = (signed.compute_txid(), signed.compute_wtxid());
    assert_eq!(daemon.calls(), 1);
    assert_eq!(resends(&h), serde_json::Value::Null, "an old-shape journal");
    // The dead end: the ordinary review refuses, reconcile only observes.
    assert!(matches!(
        coordinator.prepare_review(&context()).await,
        Err(Error::SubmissionAlreadyRecorded)
    ));
    let (_, seen) = coordinator.reconcile_sweep(&context()).await.unwrap();
    assert_eq!(seen, TransactionObservation::Absent);
    assert_eq!(daemon.calls(), 1);
    assert!(h.temp.journal()["split"].get("step2_observed").is_none());

    let review = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    let snapshot = review.snapshot();
    assert_eq!(snapshot.transaction, signed);
    assert_eq!((snapshot.txid, snapshot.wtxid), (txid, wtxid));
    assert_eq!(snapshot.fee_sats, fee);
    assert_eq!(snapshot.route, SubmissionRoute::Connect);
    assert_eq!(review.previous_attempts(), 0);
    assert_eq!(
        resends(&h),
        serde_json::Value::Null,
        "a review records nothing"
    );
    assert_eq!(daemon.calls(), 1);

    assert_eq!(
        coordinator
            .confirm_step2_resubmission(review, &context())
            .await
            .unwrap(),
        Outcome::UpstreamAccepted { txid, wtxid }
    );
    assert_eq!(
        daemon.sends(),
        vec![(
            "connect",
            txid,
            ChildNumber::from_normal_idx(INDEX).unwrap(),
            1
        )]
    );
    let journal = h.temp.journal();
    assert_eq!(resends(&h), attempts(&signed, 1));
    // Nothing recorded earlier is lost.
    assert_eq!(journal["fork_submission"]["txid"], txid.to_string());
    assert_eq!(journal["fork_submission"]["wtxid"], wtxid.to_string());
    assert_eq!(
        journal["split"]["step2_transaction"],
        serde_json::to_value(&signed).unwrap()
    );
    assert_eq!(journal["split"]["target_index"], INDEX);
    assert_eq!(
        coordinator.recorded_outcome(),
        Some(Outcome::Uncertain { txid, wtxid })
    );

    // Accepted: it left. Recorded, and the resend is gone, although
    // Connect's read does not show it yet.
    assert_eq!(journal["split"]["step2_observed"], true);
    assert!(matches!(
        coordinator.prepare_step2_resubmission(&context()).await,
        Err(ResendError::Observed)
    ));
    assert_eq!(daemon.sends().len(), 1);
}

/// A reconcile that sees the recorded step 2 on BTCB2 records the sighting,
/// on the live coordinator and on the reconcile-only restart: no resend is
/// offered afterwards, even once it is gone from the read and after a
/// restart. An absence records nothing.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_reconcile_records_a_sighting_that_ends_the_resend() {
    let connect = MockServer::start_async().await;
    let daemon = Refusing::new(None, 1);
    let Uncertain {
        h,
        mut coordinator,
        signed,
        ..
    } = uncertain(&daemon, None, &connect).await;
    let txid = signed.compute_txid();
    coordinator.reconcile_sweep(&context()).await.unwrap();
    assert!(h.temp.journal()["split"].get("step2_observed").is_none());
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, TransactionObservation::Unconfirmed { txid })]);
    coordinator.reconcile_sweep(&context()).await.unwrap();
    assert_eq!(h.temp.journal()["split"]["step2_observed"], true);
    h.chains.edit(|view| view.on_btcb2.clear());
    assert!(matches!(
        coordinator.prepare_step2_resubmission(&context()).await,
        Err(ResendError::Observed)
    ));
    drop(coordinator);
    let transport = routes_over(
        daemon.clone(),
        None,
        &connect,
        ORIGIN,
        vault_descriptor(),
        &h,
    );
    let mut reopened = reopen(&h, coins(&h.wallet), transport).await.unwrap();
    assert!(matches!(
        reopened.prepare_step2_resubmission(&context()).await,
        Err(ResendError::Observed)
    ));
    drop(reopened);

    // The reconcile-only restart records it too.
    let other_connect = MockServer::start_async().await;
    let other_daemon = Refusing::new(None, 1);
    let Uncertain {
        h,
        coordinator,
        signed,
        ..
    } = uncertain(&other_daemon, None, &other_connect).await;
    drop(coordinator);
    let txid = signed.compute_txid();
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
    let block = BlockRef {
        height: FORK_TIP,
        hash: hash(2),
    };
    h.chains.edit(|view| {
        view.on_btcb2 = vec![(txid, TransactionObservation::Confirmed { txid, block })]
    });
    reconciler.reconcile_sweep(&context()).await.unwrap();
    assert_eq!(h.temp.journal()["split"]["step2_observed"], true);
    drop(reconciler);
    h.chains.edit(|view| view.on_btcb2.clear());
    let transport = routes_over(
        other_daemon.clone(),
        None,
        &other_connect,
        ORIGIN,
        vault_descriptor(),
        &h,
    );
    let mut reopened = reopen(&h, coins(&h.wallet), transport).await.unwrap();
    assert!(matches!(
        reopened.prepare_step2_resubmission(&context()).await,
        Err(ResendError::Observed)
    ));
    assert!(daemon.sends().is_empty() && other_daemon.sends().is_empty());
}

/// The #635 dead end exactly (a backend switch during the send, refused by
/// the daemon) and recovery after a restart. The live coordinator's resend
/// refuses on the changed backend, with no fallback. Reopened, step 2 is
/// rebuilt from the journal at the current BTCB2 tip and the recorded signed
/// bytes verified: same txid, wtxid, witness, destination and fee, and no
/// reservation, signing or new construction. Only an explicit resend sends.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_dead_end_is_resent_after_a_restart_from_the_recorded_bytes() {
    let connect = MockServer::start_async().await;
    let daemon = Refusing::new(None, 0);
    *daemon.switch.lock().unwrap() = Some(9);
    let Uncertain {
        h,
        mut coordinator,
        signed,
        fee,
        ..
    } = uncertain(&daemon, None, &connect).await;
    let (txid, wtxid) = (signed.compute_txid(), signed.compute_wtxid());
    assert!(matches!(
        coordinator.prepare_step2_resubmission(&context()).await,
        Err(ResendError::Coordinator(Error::Preflight(
            claim_preflight::Error::BackendChanged
        )))
    ));
    assert_eq!(resends(&h), serde_json::Value::Null);
    drop(coordinator);

    let transport = || {
        routes_over(
            daemon.clone(),
            None,
            &connect,
            ORIGIN,
            vault_descriptor(),
            &h,
        )
    };
    let journal = h.temp.journal();
    // Refused: a claimed coin missing, another Vault, another origin, a
    // recorded locktime above the current BTCB2 tip.
    let mut missing = coins(&h.wallet);
    missing.pop();
    assert!(matches!(
        reopen(&h, missing, transport()).await,
        Err(Error::InvalidBinding)
    ));
    for (origin, descriptor) in [
        (ORIGIN, other_vault()),
        ("https://other.example/", vault_descriptor()),
    ] {
        let other = routes_over(daemon.clone(), None, &connect, origin, descriptor, &h);
        assert!(matches!(
            reopen(&h, coins(&h.wallet), other).await,
            Err(Error::InvalidBinding)
        ));
    }
    h.chains.edit(|view| view.fork_tip = FORK_TIP - 1);
    assert!(matches!(
        reopen(&h, coins(&h.wallet), transport()).await,
        Err(Error::InvalidBinding)
    ));
    h.chains.edit(|view| view.fork_tip = FORK_TIP);
    assert_eq!(h.temp.journal(), journal, "refusals wrote nothing");

    let mut coordinator = reopen(&h, coins(&h.wallet), transport()).await.unwrap();
    assert_eq!(coordinator.transaction(), &signed, "the recorded bytes");
    assert_eq!(
        coordinator.recorded_outcome(),
        Some(Outcome::Uncertain { txid, wtxid })
    );
    // Reopening restores no review authority.
    assert!(matches!(
        coordinator.prepare_review(&context()).await,
        Err(Error::SubmissionAlreadyRecorded)
    ));
    assert_eq!(daemon.calls(), 1);
    let review = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    let snapshot = review.snapshot();
    assert_eq!(snapshot.transaction, signed);
    assert_eq!(snapshot.fee_sats, fee);
    assert_eq!(
        snapshot.transaction.output[0].script_pubkey,
        address(&vault(), INDEX).script_pubkey()
    );
    assert_eq!(
        coordinator
            .confirm_step2_resubmission(review, &context())
            .await
            .unwrap(),
        Outcome::UpstreamAccepted { txid, wtxid }
    );
    assert_eq!(
        daemon.sends(),
        vec![(
            "connect",
            txid,
            ChildNumber::from_normal_idx(INDEX).unwrap(),
            9
        )]
    );
    let after = h.temp.journal();
    assert_eq!(after["split"]["step2_observed"], true, "it left");
    assert_eq!(resends(&h), attempts(&signed, 1));
    assert_eq!(
        after["split"]["target_index"],
        journal["split"]["target_index"]
    );
    assert_eq!(after["fork_sweep"], journal["fork_sweep"]);
    assert_eq!(
        after["split"]["step2_transaction"],
        journal["split"]["step2_transaction"]
    );
}

/// A recorded signed step 2 whose journal is consistent (its submission
/// names its own txid and wtxid, it signs the recorded sweep) but whose
/// signature does not verify is refused on restart: the core verifier, not
/// the journal's shape checks, establishes the bytes. Without a recorded
/// submission there is nothing to resume.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_restart_refuses_recorded_bytes_that_do_not_verify() {
    let connect = MockServer::start_async().await;
    let daemon = Refusing::new(None, 1);
    let Uncertain {
        h,
        coordinator,
        signed,
        ..
    } = uncertain(&daemon, None, &connect).await;
    drop(coordinator);
    let mut forged = signed.clone();
    let mut script = forged.input[0].script_sig.to_bytes();
    script[10] ^= 1;
    forged.input[0].script_sig = coincube_core::miniscript::bitcoin::ScriptBuf::from_bytes(script);
    h.temp.rewrite(|intent| {
        intent["split"]["step2_transaction"] = serde_json::to_value(&forged).unwrap();
        intent["fork_submission"] = json!({
            "txid": forged.compute_txid().to_string(),
            "wtxid": forged.compute_wtxid().to_string(),
        });
    });
    let transport = routes_over(
        daemon.clone(),
        None,
        &connect,
        ORIGIN,
        vault_descriptor(),
        &h,
    );
    assert!(matches!(
        reopen(&h, coins(&h.wallet), transport).await,
        Err(Error::InvalidBinding)
    ));

    let unsubmitted = Step2::new().await;
    let Step2 {
        h: unsubmitted,
        preparation,
        ..
    } = unsubmitted;
    drop(preparation);
    let transport = routes_over(
        daemon.clone(),
        None,
        &connect,
        ORIGIN,
        vault_descriptor(),
        &unsubmitted,
    );
    assert!(matches!(
        reopen(&unsubmitted, coins(&unsubmitted.wallet), transport).await,
        Err(Error::InvalidBinding)
    ));
    assert_eq!(daemon.calls(), 1);
}

/// Fresh evidence, or no resend: the recorded step 2 absent from BTCB2,
/// every claimed coin unspent there, step 1 six deep on Bitcoin and absent
/// from BTCB2, RDTS outside the margin, fresh reads, a stable view and an
/// accepted preflight on the route. Each refusal records nothing and sends
/// nothing. A step 2 seen on BTCB2 is recorded as observed, which survives
/// its disappearance from the read and a restart.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_resend_needs_fresh_absence_unspent_coins_and_depth() {
    let connect = MockServer::start_async().await;
    let daemon = Refusing::new(None, 1);
    let Uncertain {
        h,
        mut coordinator,
        signed,
        preflight,
        ..
    } = uncertain(&daemon, None, &connect).await;
    let txid = signed.compute_txid();
    let spent = h.prevouts()[1];
    let journal = h.temp.journal();
    type Edit = Box<dyn Fn(&mut View)>;
    let cases: Vec<(&str, Edit, Edit)> = vec![
        (
            "claimed coin spent",
            Box::new(move |v: &mut View| {
                v.unspent.remove(&spent);
            }),
            Box::new(move |v: &mut View| {
                v.unspent.insert(spent);
            }),
        ),
        (
            "unspent read unavailable",
            Box::new(|v: &mut View| v.unspent_fails = true),
            Box::new(|v: &mut View| v.unspent_fails = false),
        ),
        (
            "step 1 five deep",
            Box::new(|v: &mut View| v.set_depth(5)),
            Box::new(|v: &mut View| v.set_depth(6)),
        ),
        (
            "step 1 on BTCB2",
            // The synthetic BTCB2 shows every txid once `on_fork` is set;
            // keep the step-2 read absent.
            Box::new(move |v: &mut View| {
                v.on_fork = true;
                v.on_btcb2 = vec![(txid, TransactionObservation::Absent)];
            }),
            Box::new(|v: &mut View| {
                v.on_fork = false;
                v.on_btcb2.clear();
            }),
        ),
        (
            "RDTS margin",
            Box::new(|v: &mut View| v.rdts_expiry = MTP + 600),
            Box::new(|v: &mut View| v.rdts_expiry = 20_000),
        ),
        (
            "stale reads",
            Box::new(|v: &mut View| v.clock_offset = 3_600),
            Box::new(|v: &mut View| v.clock_offset = 0),
        ),
        (
            "a new block between the collections",
            Box::new(|v: &mut View| v.between = Some(Box::new(|v: &mut View| v.set_depth(7)))),
            Box::new(|v: &mut View| v.set_depth(6)),
        ),
    ];
    for (case, set, reset) in cases {
        h.chains.edit(|view| set(view));
        let result = coordinator.prepare_step2_resubmission(&context()).await;
        match case {
            "claimed coin spent" => assert!(
                matches!(result, Err(ResendError::ClaimedCoinSpent(o)) if o == spent),
                "{}",
                case
            ),
            "unspent read unavailable" => assert!(
                matches!(
                    result,
                    Err(ResendError::Unavailable(_, FailureKind::Http(400)))
                ),
                "{}",
                case
            ),
            "stale reads" => assert!(
                matches!(result, Err(ResendError::Coordinator(Error::Observation(_)))),
                "{}",
                case
            ),
            "a new block between the collections" => assert!(
                matches!(result, Err(ResendError::Coordinator(Error::ChangedReview))),
                "{}",
                case
            ),
            _ => assert!(
                matches!(result, Err(ResendError::Coordinator(Error::NotReady(_)))),
                "{}: {:?}",
                case,
                result.err()
            ),
        }
        h.chains.edit(|view| reset(view));
        assert_eq!(h.temp.journal(), journal, "{}: nothing recorded", case);
    }
    // A refused preflight on the route; no fallback.
    httpmock::Mock::new(preflight, &connect)
        .delete_async()
        .await;
    let refusing = mock_preflight(&connect, &signed, false).await;
    assert!(matches!(
        coordinator.prepare_step2_resubmission(&context()).await,
        Err(ResendError::Coordinator(Error::PolicyRejected(_)))
    ));
    httpmock::Mock::new(refusing, &connect).delete_async().await;
    mock_preflight(&connect, &signed, true).await;
    assert_eq!(h.temp.journal(), journal);
    assert_eq!(daemon.calls(), 1);
    // The evidence is good again.
    coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();

    // Seen confirmed on BTCB2: it left. Recorded, and refused for good.
    let block = BlockRef {
        height: FORK_TIP,
        hash: hash(2),
    };
    h.chains.edit(|view| {
        view.on_btcb2 = vec![(txid, TransactionObservation::Confirmed { txid, block })]
    });
    assert!(matches!(
        coordinator.prepare_step2_resubmission(&context()).await,
        Err(ResendError::Observed)
    ));
    assert_eq!(h.temp.journal()["split"]["step2_observed"], true);
    h.chains.edit(|view| view.on_btcb2.clear());
    assert!(matches!(
        coordinator.prepare_step2_resubmission(&context()).await,
        Err(ResendError::Observed)
    ));
    drop(coordinator);
    let transport = routes_over(
        daemon.clone(),
        None,
        &connect,
        ORIGIN,
        vault_descriptor(),
        &h,
    );
    let mut reopened = reopen(&h, coins(&h.wallet), transport).await.unwrap();
    assert!(matches!(
        reopened.prepare_step2_resubmission(&context()).await,
        Err(ResendError::Observed)
    ));
    assert!(daemon.sends().is_empty());
    assert_eq!(resends(&h), serde_json::Value::Null);
}

/// The review is one use and bound: nothing to resend before a submission;
/// an older review, an expired one, another coordinator's, one whose view,
/// coins or backend changed before confirmation, and one from a revoked or
/// replaced session all refuse, record nothing and send nothing.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_resend_review_is_one_use_and_bound() {
    let connect = MockServer::start_async().await;
    // Before any submission: the ordinary review applies.
    let fresh = Refusing::new(None, 0);
    let s = Step2::new().await;
    let signed = s.signed_tx();
    mock_preflight(&connect, &signed, true).await;
    let transport = routes_over(
        fresh.clone(),
        None,
        &connect,
        ORIGIN,
        vault_descriptor(),
        &s.h,
    );
    let psbt = s.signed();
    let coins_now = coins(&s.h.wallet);
    let mut unsubmitted = s
        .preparation
        .finish_with(&context(), &psbt, &coins_now, transport)
        .unwrap();
    assert!(matches!(
        unsubmitted.prepare_step2_resubmission(&context()).await,
        Err(ResendError::NotRecorded)
    ));
    drop(unsubmitted);

    let daemon = Refusing::new(None, 1);
    let Uncertain {
        h, mut coordinator, ..
    } = uncertain(&daemon, None, &connect).await;
    let other_connect = MockServer::start_async().await;
    let other_daemon = Refusing::new(None, 1);
    let Uncertain {
        h: _other_h,
        coordinator: mut other,
        ..
    } = uncertain(&other_daemon, None, &other_connect).await;
    let journal = h.temp.journal();
    let refused = |result: Result<Outcome, ResendError>| result.err();

    // Another coordinator's review. Both coordinators went through the same
    // review and confirmation and have each prepared one resend review, so
    // the revisions agree: only the coordinator identity refuses it.
    let _own = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    let foreign = other.prepare_step2_resubmission(&context()).await.unwrap();
    assert!(matches!(
        refused(
            coordinator
                .confirm_step2_resubmission(foreign, &context())
                .await
        ),
        Some(ResendError::Coordinator(Error::InvalidReview))
    ));
    // An older review: a later review supersedes it.
    let older = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    let _later = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    assert!(matches!(
        refused(
            coordinator
                .confirm_step2_resubmission(older, &context())
                .await
        ),
        Some(ResendError::Coordinator(Error::InvalidReview))
    ));
    // Expired.
    let mut expired: Step2ResubmissionReview = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    expired.expire_for_test();
    assert!(matches!(
        refused(
            coordinator
                .confirm_step2_resubmission(expired, &context())
                .await
        ),
        Some(ResendError::Coordinator(Error::ExpiredEvidence))
    ));
    // The view changed before confirmation.
    let review = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    h.chains.edit(|view| view.set_depth(7));
    assert!(matches!(
        refused(
            coordinator
                .confirm_step2_resubmission(review, &context())
                .await
        ),
        Some(ResendError::Coordinator(Error::ChangedReview))
    ));
    // A claimed coin spent before confirmation.
    let spent = h.prevouts()[0];
    let review = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    h.chains.edit(|view| {
        view.unspent.remove(&spent);
    });
    assert!(matches!(
        refused(coordinator.confirm_step2_resubmission(review, &context()).await),
        Some(ResendError::ClaimedCoinSpent(o)) if o == spent
    ));
    h.chains.edit(|view| {
        view.unspent.insert(spent);
    });
    // The backend changed before confirmation.
    let review = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    daemon.vault.binding.store(2, Ordering::SeqCst);
    assert!(matches!(
        refused(
            coordinator
                .confirm_step2_resubmission(review, &context())
                .await
        ),
        Some(ResendError::Coordinator(Error::Preflight(
            claim_preflight::Error::BackendChanged
        )))
    ));
    daemon.vault.binding.store(1, Ordering::SeqCst);
    // Another session context.
    let review = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    let mut elsewhere = context();
    elsewhere.account = "other".into();
    assert!(matches!(
        refused(
            coordinator
                .confirm_step2_resubmission(review, &elsewhere)
                .await
        ),
        Some(ResendError::Coordinator(Error::Revoked))
    ));
    assert!(matches!(
        coordinator.prepare_step2_resubmission(&context()).await,
        Err(ResendError::Coordinator(Error::Revoked))
    ));
    assert_eq!(h.temp.journal(), journal, "nothing recorded");
    assert_eq!(daemon.calls(), 1, "nothing sent");

    // A generation change (logout, Cube close) between review and
    // confirmation; and a revoked coordinator.
    let review = other.prepare_step2_resubmission(&context()).await.unwrap();
    other.revoker().revoke();
    assert!(matches!(
        refused(other.confirm_step2_resubmission(review, &context()).await),
        Some(ResendError::Coordinator(Error::Revoked))
    ));
    drop(other);
    let daemon3 = Refusing::new(None, 1);
    let third_connect = MockServer::start_async().await;
    let Uncertain {
        h: h3,
        coordinator: mut third,
        ..
    } = uncertain(&daemon3, None, &third_connect).await;
    let review = third.prepare_step2_resubmission(&context()).await.unwrap();
    h3.sender.send(8).unwrap();
    assert!(matches!(
        refused(third.confirm_step2_resubmission(review, &context()).await),
        Some(ResendError::Coordinator(Error::Revoked))
    ));
    assert_eq!(resends(&h3), serde_json::Value::Null);
    assert_eq!(other_daemon.calls() + daemon3.calls(), 2, "nothing sent");
}

/// Each permitted attempt is recorded before its one send, and a failed
/// write sends nothing. A resend refused again is `Uncertain` again, with no
/// automatic retry; the next one needs its own review. The journal bounds
/// the number of resends.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_resend_records_each_attempt_before_its_one_send() {
    use std::os::unix::fs::PermissionsExt;
    let connect = MockServer::start_async().await;
    let daemon = Refusing::new(None, 2);
    let Uncertain {
        h,
        mut coordinator,
        signed,
        ..
    } = uncertain(&daemon, None, &connect).await;
    let (txid, wtxid) = (signed.compute_txid(), signed.compute_wtxid());
    // Refused again: Uncertain, recorded, not retried.
    let review = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    assert_eq!(
        coordinator
            .confirm_step2_resubmission(review, &context())
            .await
            .unwrap(),
        Outcome::Uncertain { txid, wtxid }
    );
    assert_eq!(daemon.calls(), 2);
    assert_eq!(resends(&h), attempts(&signed, 1));
    // The next resend has its own review, which sees the recorded attempt.
    let review = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    assert_eq!(review.previous_attempts(), 1);
    // Storage refuses the write: no send.
    let permissions =
        |mode| std::fs::set_permissions(&h.temp.0, std::fs::Permissions::from_mode(mode)).unwrap();
    permissions(0o500);
    let result = coordinator
        .confirm_step2_resubmission(review, &context())
        .await;
    permissions(0o700);
    assert!(
        matches!(
            result,
            Err(ResendError::Coordinator(Error::Journal(
                claim_workflow::Error::Io(_)
            )))
        ),
        "{:?}",
        result
    );
    assert_eq!(daemon.calls(), 2, "nothing sent");
    assert_eq!(resends(&h), attempts(&signed, 1));
    drop(coordinator);

    // Reopened (the failed write left the file as it was): one more resend
    // is recorded, then sent once.
    let transport = || {
        routes_over(
            daemon.clone(),
            None,
            &connect,
            ORIGIN,
            vault_descriptor(),
            &h,
        )
    };
    let mut coordinator = reopen(&h, coins(&h.wallet), transport()).await.unwrap();
    let review = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    assert_eq!(review.previous_attempts(), 1);
    assert_eq!(
        coordinator
            .confirm_step2_resubmission(review, &context())
            .await
            .unwrap(),
        Outcome::UpstreamAccepted { txid, wtxid }
    );
    assert_eq!(daemon.calls(), 3);
    assert_eq!(daemon.sends().len(), 1);
    assert_eq!(resends(&h), attempts(&signed, 2));
    drop(coordinator);

    // The bound: a journal at the limit refuses another; one past it is
    // not a valid journal.
    let max = claim_workflow::MAX_SPLIT_STEP2_RESUBMISSIONS;
    h.temp.rewrite(|intent| {
        // The accepted resend above recorded a sighting; drop it to reach
        // the bound alone.
        intent["split"]
            .as_object_mut()
            .unwrap()
            .remove("step2_observed");
        intent["split"]["step2_resubmissions"] = attempts(&signed, max)
    });
    let mut coordinator = reopen(&h, coins(&h.wallet), transport()).await.unwrap();
    assert!(matches!(
        coordinator.prepare_step2_resubmission(&context()).await,
        Err(ResendError::AttemptsExhausted)
    ));
    drop(coordinator);
    h.temp
        .rewrite(|intent| intent["split"]["step2_resubmissions"] = attempts(&signed, max + 1));
    assert!(reopen(&h, coins(&h.wallet), transport()).await.is_err());
    assert_eq!(daemon.calls(), 3);
}

/// The P4 node route: a resend is preflighted on the bound node at the
/// Connect-observed BTCB2 tip and sent there once; Connect's preflight is
/// never asked.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_resend_on_the_node_route_stays_on_the_node() {
    let node = MockServer::start_async().await;
    let connect = MockServer::start_async().await;
    let daemon = Refusing::new(Some(node_config(&node)), 1);
    // Construction and RFC 6979 signing are deterministic: this is the
    // step 2 the coordinator will hold.
    let s = Step2::new().await;
    let expected = s.signed_tx();
    drop(s);
    knots(&node, &expected, hash(2), true).await;
    let Uncertain {
        h: _h,
        mut coordinator,
        signed,
        preflight,
        ..
    } = uncertain(&daemon, Some(node_config(&node)), &connect).await;
    assert_eq!(signed, expected);
    let (txid, wtxid) = (signed.compute_txid(), signed.compute_wtxid());
    let review = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    assert!(matches!(
        review.snapshot().route,
        SubmissionRoute::BitcoinNode { address, .. } if address == *node.address()
    ));
    assert_eq!(
        coordinator
            .confirm_step2_resubmission(review, &context())
            .await
            .unwrap(),
        Outcome::UpstreamAccepted { txid, wtxid }
    );
    assert_eq!(
        daemon.sends(),
        vec![(
            "node",
            txid,
            ChildNumber::from_normal_idx(INDEX).unwrap(),
            1
        )]
    );
    assert_eq!(
        httpmock::Mock::new(preflight, &connect).hits_async().await,
        0,
        "Connect's preflight is never asked on the node route"
    );
}

/// A resend whose send never answers is cancelled by a generation change
/// (logout, Cube close): `Uncertain` again, its attempt recorded, nothing
/// retried, and the coordinator revoked.
#[tokio::test(flavor = "multi_thread")]
async fn split_step2_resend_cancelled_mid_send_stays_uncertain() {
    let connect = MockServer::start_async().await;
    let daemon = Refusing::new(None, 1);
    let Uncertain {
        h,
        mut coordinator,
        signed,
        ..
    } = uncertain(&daemon, None, &connect).await;
    let review = coordinator
        .prepare_step2_resubmission(&context())
        .await
        .unwrap();
    daemon.hang.store(true, Ordering::SeqCst);
    let session = context();
    let (outcome, _) = tokio::join!(
        coordinator.confirm_step2_resubmission(review, &session),
        async {
            while daemon.calls() < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            h.sender.send(8).unwrap();
        }
    );
    assert_eq!(
        outcome.unwrap(),
        Outcome::Uncertain {
            txid: signed.compute_txid(),
            wtxid: signed.compute_wtxid(),
        }
    );
    assert_eq!(resends(&h), attempts(&signed, 1));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(daemon.calls(), 2, "no retry");
    assert!(daemon.sends().is_empty());
    assert!(matches!(
        coordinator.prepare_step2_resubmission(&context()).await,
        Err(ResendError::Coordinator(Error::Revoked))
    ));
}
