//! #636 P3-1: the production preparation driver's own logic, over a fake
//! [`PrepCore`]: the used-target replacement, the token's one-build
//! lifetime and the signed-file check.
use super::*;
use coincube_core::foreign_split::FinalizeError;
use std::{collections::VecDeque, sync::Mutex};

#[derive(Default)]
struct Calls {
    reserves: usize,
    proves: usize,
    constructs: usize,
}
struct FakeCore {
    calls: Arc<Mutex<Calls>>,
    needs: Option<bool>,
    recorded: Option<u32>,
    proofs: VecDeque<Result<(), TargetError>>,
    check_fails: bool,
    construct_fails: bool,
    signed: Option<FinalizeError>,
    /// Keeps the minted tokens' check counters alive.
    live: Vec<Arc<std::sync::atomic::AtomicU64>>,
}
impl FakeCore {
    fn new() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Calls::default())),
            needs: Some(false),
            recorded: Some(3),
            proofs: VecDeque::new(),
            check_fails: false,
            construct_fails: false,
            signed: None,
            live: Vec::new(),
        }
    }
}
#[async_trait]
impl PrepCore for FakeCore {
    fn revoke_handle(&self) -> RevokeHandle {
        Arc::new(|| {})
    }
    fn needs_reservation(&self) -> Result<bool, CoordinatorError> {
        self.needs.ok_or(CoordinatorError::Journal(
            claim_workflow::Error::InvalidJournal,
        ))
    }
    fn recorded_target(&self) -> Result<Option<u32>, CoordinatorError> {
        Ok(self.recorded)
    }
    async fn check_signing(
        &mut self,
        _: &Context,
    ) -> Result<ForeignStep2Authorization, SplitCheckError> {
        if self.check_fails {
            return Err(SplitCheckError::Coordinator(CoordinatorError::NotReady(
                Assessment::WaitingForDepth { confirmations: 5 },
            )));
        }
        let (token, live) = ForeignStep2Authorization::for_test(
            &[OutPoint::new(Txid::from_byte_array([1; 32]), 0)],
            Txid::from_byte_array([2; 32]),
            watch::channel(7).1,
        );
        self.live.push(live);
        Ok(token)
    }
    async fn reserve(&mut self, _: &Context) -> Result<u32, TargetError> {
        self.calls.lock().unwrap().reserves += 1;
        // The journal admits only a strictly higher replacement.
        let next = self.recorded.map_or(3, |index| index + 1);
        self.recorded = Some(next);
        Ok(next)
    }
    async fn prove(&mut self, _: &Context) -> Result<(), TargetError> {
        self.calls.lock().unwrap().proves += 1;
        self.proofs.pop_front().unwrap_or(Ok(()))
    }
    async fn construct(
        &mut self,
        _: &Context,
        token: ForeignStep2Authorization,
        _: Vec<SplitCoin>,
    ) -> Result<Psbt, Step2Error> {
        self.calls.lock().unwrap().constructs += 1;
        drop(token);
        if self.construct_fails {
            return Err(Step2Error::FeeUnavailable);
        }
        Ok(Psbt::from_unsigned_tx(Transaction {
            version: transaction::Version::TWO,
            lock_time: LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![],
        })
        .unwrap())
    }
    fn check_signed(&self, _: &Psbt, _: &[SplitCoin]) -> Result<(), FinalizeError> {
        match self.signed {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}
/// (reserves, proves), under one lock.
fn counts(calls: &Arc<Mutex<Calls>>) -> (usize, usize) {
    let calls = calls.lock().unwrap();
    (calls.reserves, calls.proves)
}
fn driver(core: FakeCore) -> PreparationDriver<FakeCore> {
    PreparationDriver {
        core,
        token: None,
        finish: None,
    }
}

/// A recorded target proven used is replaced exactly once (the journal
/// gives a strictly higher index) and proven; a second used target refuses
/// with no third reservation.
#[tokio::test]
async fn ensure_target_replaces_a_used_target_once() {
    let mut core = FakeCore::new();
    core.proofs = VecDeque::from([Err(TargetError::Used(ChainId::Bitcoin)), Ok(())]);
    let calls = core.calls.clone();
    let mut d = driver(core);
    assert_eq!(d.ensure_target_inner(&context()).await, Ok(4));
    assert_eq!(counts(&calls), (1, 2));

    let mut core = FakeCore::new();
    core.proofs = VecDeque::from([
        Err(TargetError::Used(ChainId::Bitcoin)),
        Err(TargetError::Used(ChainId::BitcoinBlake2b)),
    ]);
    let calls = core.calls.clone();
    let mut d = driver(core);
    let refused = d.ensure_target_inner(&context()).await.unwrap_err();
    assert!(refused.reason.contains("Bitcoin Blake2b"));
    assert_eq!(counts(&calls), (1, 2));
}

/// An unavailable or stale proof, or a target that is not this Vault's,
/// never triggers a replacement; a journal error is never read as "no
/// reservation needed".
#[tokio::test]
async fn ensure_target_never_replaces_on_an_unproven_target() {
    for error in [
        TargetError::Unavailable(ChainId::Bitcoin, FailureKind::Stale),
        TargetError::Unavailable(ChainId::BitcoinBlake2b, FailureKind::Http(503)),
        TargetError::NotTargetVault,
    ] {
        let mut core = FakeCore::new();
        core.proofs = VecDeque::from([Err(error)]);
        let calls = core.calls.clone();
        let mut d = driver(core);
        assert!(d.ensure_target_inner(&context()).await.is_err());
        assert_eq!(calls.lock().unwrap().reserves, 0);
    }
    let mut core = FakeCore::new();
    core.needs = None;
    let calls = core.calls.clone();
    let mut d = driver(core);
    assert!(d.ensure_target_inner(&context()).await.is_err());
    assert_eq!(counts(&calls), (0, 0));
    // No reservation yet: one is made, then proven.
    let mut core = FakeCore::new();
    core.needs = Some(true);
    core.recorded = None;
    let calls = core.calls.clone();
    let mut d = driver(core);
    assert_eq!(d.ensure_target_inner(&context()).await, Ok(3));
    assert_eq!(counts(&calls), (1, 1));
}

/// One build per check: without a check it refuses; a check's token is
/// consumed by one build even when construction fails; a failed check
/// clears an earlier token.
#[tokio::test]
async fn build_needs_its_own_successful_check() {
    let core = FakeCore::new();
    let calls = core.calls.clone();
    let mut d = driver(core);
    assert!(d.build_inner(&context(), Vec::new()).await.is_err());
    assert_eq!(calls.lock().unwrap().constructs, 0);

    d.core.construct_fails = true;
    assert!(d.check_inner(&context()).await.is_ok());
    assert!(d.build_inner(&context(), Vec::new()).await.is_err());
    assert_eq!(calls.lock().unwrap().constructs, 1);
    assert!(d.build_inner(&context(), Vec::new()).await.is_err());
    assert_eq!(
        calls.lock().unwrap().constructs,
        1,
        "the token was consumed"
    );

    d.core.construct_fails = false;
    assert!(d.check_inner(&context()).await.is_ok());
    d.core.check_fails = true;
    assert!(d.check_inner(&context()).await.is_err());
    assert!(d.build_inner(&context(), Vec::new()).await.is_err());
    assert_eq!(
        calls.lock().unwrap().constructs,
        1,
        "the failed check cleared it"
    );

    d.core.check_fails = false;
    assert!(d.check_inner(&context()).await.is_ok());
    assert!(d.build_inner(&context(), Vec::new()).await.is_ok());
    assert_eq!(calls.lock().unwrap().constructs, 2);
}

/// A signed-file check: complete, partial (more signatures needed) or a
/// mismatch that refuses.
#[test]
fn verify_signed_distinguishes_partial_from_wrong() {
    let psbt = Psbt::from_unsigned_tx(Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn::default()],
        output: vec![],
    })
    .unwrap();
    let mut d = driver(FakeCore::new());
    assert_eq!(d.verify_inner(&psbt, &[]), Ok(true));
    d.core.signed = Some(FinalizeError::Unsatisfied);
    assert_eq!(d.verify_inner(&psbt, &[]), Ok(false));
    d.core.signed = Some(FinalizeError::ConstructionChanged);
    assert!(d.verify_inner(&psbt, &[]).is_err());
}

/// #568 B5b: the reconciler driver's completion over a fake
/// [`CompletionCore`]: what it was asked, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Call {
    Check,
    Persist,
    Forget,
}
struct FakeEvidence {
    live: bool,
}
struct FakeCompletion {
    calls: Arc<Mutex<Vec<Call>>>,
    /// The check: `None` mints nothing; `Some(live)` mints evidence that is
    /// live (or not) when its record is refused.
    minted: Option<bool>,
    /// #656 F1: the check itself refused.
    check_fails: Option<CoordinatorError>,
    persist_fails: bool,
    forget_fails: Option<CoordinatorError>,
    /// #662 R10: the D17 recheck refuses with this.
    stands_fails: Option<CoordinatorError>,
}
impl FakeCompletion {
    fn new() -> Self {
        Self {
            calls: Arc::default(),
            minted: Some(true),
            check_fails: None,
            persist_fails: false,
            forget_fails: None,
            stands_fails: None,
        }
    }
}
#[async_trait]
impl CompletionCore for FakeCompletion {
    type Evidence = FakeEvidence;
    async fn check(&mut self, _: &Context) -> Result<Option<FakeEvidence>, CoordinatorError> {
        self.calls.lock().unwrap().push(Call::Check);
        if let Some(error) = self.check_fails.take() {
            return Err(error);
        }
        Ok(self.minted.map(|live| FakeEvidence { live }))
    }
    fn record(_: &FakeEvidence) -> SplitFromRecord {
        SplitFromRecord {
            descriptor_digest: sha256::Hash::hash(b"split source"),
            completed_height: 1_000,
            step2_txid: Txid::from_byte_array([5; 32]),
        }
    }
    fn live(evidence: &FakeEvidence) -> bool {
        evidence.live
    }
    async fn persist(&self, _: &FakeEvidence, _: &CompletionSite) -> Result<(), SettingsError> {
        self.calls.lock().unwrap().push(Call::Persist);
        if self.persist_fails {
            return Err(SettingsError::Unexpected(
                "Claim Cube no longer matches its Vault".into(),
            ));
        }
        Ok(())
    }
    fn forget(&mut self, _: FakeEvidence, _: &Context) -> Result<(), CoordinatorError> {
        self.calls.lock().unwrap().push(Call::Forget);
        match self.forget_fails.take() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    async fn stands(
        &mut self,
        _: &Context,
        _: &CompletionSite,
    ) -> Result<SplitCompletionReconciliation, CoordinatorError> {
        match self.stands_fails.take() {
            Some(error) => Err(error),
            None => Ok(SplitCompletionReconciliation::Standing {
                status: Status::Unchecked,
                transaction: TransactionObservation::Absent,
            }),
        }
    }
}
fn site() -> CompletionSite {
    CompletionSite {
        root: crate::dir::CoincubeDirectory::new(std::env::temp_dir().join("split-b5b-unused")),
        target: CompletionTarget {
            cube_id: TARGET.into(),
            vault_wallet_id: crate::app::settings::WalletId {
                timestamp: None,
                descriptor_checksum: "abcdefgh".into(),
            },
            vault_fingerprint: "f5acc2fd".into(),
        },
    }
}
fn recon_driver(core: FakeCompletion) -> ReconcilerDriver<FakeCompletion> {
    ReconcilerDriver {
        core: Some(core),
        site: Some(site()),
        revoke: Arc::new(|| {}),
        reconfirmation: None,
        generation: watch::channel(0).1,
        expected: 0,
    }
}

/// D18 through the driver: check, then the record, then the deletion, and
/// the history row from the evidence. Nothing is recorded without evidence;
/// a refused record deletes nothing and reads "check again" whether the
/// evidence lapsed (#645 P3-1) or the Cube's settings refused it (never the
/// settings layer's "Claim Cube" message); a lapsed deletion after the
/// record is "check again" too. Without a Vault in the Cube's settings
/// nothing is checked. Every refusal but that one is retryable.
#[tokio::test(flavor = "multi_thread")]
async fn recon_driver_persists_before_forgetting() {
    let core = FakeCompletion::new();
    let calls = core.calls.clone();
    let mut d = recon_driver(core);
    let completion = d.complete_inner(&context()).await.unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        [Call::Check, Call::Persist, Call::Forget]
    );
    assert_eq!(completion.completed_height, 1_000);
    assert_eq!(completion.step2_txid, Txid::from_byte_array([5; 32]));
    assert!(
        d.core.is_some(),
        "the reconciler is back after the deletion"
    );

    let refused = |copy: &str, expected: &[Call], core: FakeCompletion| {
        let calls = core.calls.clone();
        let copy = copy.to_string();
        let expected = expected.to_vec();
        async move {
            let mut d = recon_driver(core);
            let refusal = d.complete_inner(&context()).await.unwrap_err();
            assert_eq!(refusal.reason, copy);
            assert!(refusal.retry, "{}", copy);
            assert!(!refusal.reason.contains("Claim"));
            assert_eq!(*calls.lock().unwrap(), expected, "{}", copy);
            assert!(d.core.is_some());
        }
    };
    let mut core = FakeCompletion::new();
    core.minted = None;
    refused(COMPLETION_NOT_YET, &[Call::Check], core).await;
    let mut core = FakeCompletion::new();
    core.persist_fails = true;
    refused(COMPLETION_NOT_RECORDED, &[Call::Check, Call::Persist], core).await;
    let mut core = FakeCompletion::new();
    core.persist_fails = true;
    core.minted = Some(false);
    refused(COMPLETION_EXPIRED, &[Call::Check, Call::Persist], core).await;
    let mut core = FakeCompletion::new();
    core.forget_fails = Some(CoordinatorError::ExpiredEvidence);
    refused(
        COMPLETION_NOT_FORGOTTEN,
        &[Call::Check, Call::Persist, Call::Forget],
        core,
    )
    .await;
    // #656 F1: the check's own evidence lapsing, and the completion
    // record's persistence failing, read as Split's lines, never the Claim
    // or submission copy.
    let mut core = FakeCompletion::new();
    core.check_fails = Some(CoordinatorError::ExpiredEvidence);
    refused(COMPLETION_CHECK_EXPIRED, &[Call::Check], core).await;
    let mut core = FakeCompletion::new();
    core.forget_fails = Some(CoordinatorError::CompletionPersistence(
        "Claim Cube is missing or ambiguous".into(),
    ));
    refused(
        COMPLETION_RECORD_UNAVAILABLE,
        &[Call::Check, Call::Persist, Call::Forget],
        core,
    )
    .await;

    // No Vault named in the Cube's settings: final, nothing checked.
    let core = FakeCompletion::new();
    let calls = core.calls.clone();
    let mut d = recon_driver(core);
    d.site = None;
    let refusal = d.complete_inner(&context()).await.unwrap_err();
    assert_eq!(refusal.reason, COMPLETION_NO_VAULT);
    assert!(!refusal.retry);
    assert!(calls.lock().unwrap().is_empty());
    // A driver that lost its reconciler asks for a restart.
    let mut d = recon_driver(FakeCompletion::new());
    d.core = None;
    let refusal = d.complete_inner(&context()).await.unwrap_err();
    assert_eq!(refusal.recovery, Step2Recovery::Restart);
    assert_eq!(refusal.reason, COMPLETION_INTERRUPTED);
}

/// #662 R10: the D17 recheck through the driver. Its refusals read as the
/// completion's own lines: expired evidence says the record may already be
/// removed (never "complete the split", never Claim's submission copy), and
/// the record's persistence is the completion record's line (never "Claim
/// status"). Both are retryable. CF: the recheck maps through
/// `describe_check`.
#[tokio::test(flavor = "multi_thread")]
async fn recon_driver_recheck_refusals_have_split_copy() {
    let mut d = recon_driver(FakeCompletion::new());
    assert!(matches!(
        d.stands_inner(&context()).await,
        Ok(CompletionStanding::Standing { .. })
    ));
    for (error, copy) in [
        (
            CoordinatorError::ExpiredEvidence,
            COMPLETION_RECHECK_EXPIRED,
        ),
        (
            CoordinatorError::CompletionPersistence("Claim Cube is missing or ambiguous".into()),
            COMPLETION_RECORD_UNAVAILABLE,
        ),
    ] {
        let mut core = FakeCompletion::new();
        core.stands_fails = Some(error);
        let mut d = recon_driver(core);
        let refusal = d.stands_inner(&context()).await.unwrap_err();
        assert_eq!(refusal.reason, copy);
        assert!(refusal.retry);
        assert!(!refusal.reason.contains("Claim"));
        assert!(!refusal.reason.contains("complete the split"));
        assert!(d.core.is_some());
    }
    let mut d = recon_driver(FakeCompletion::new());
    d.site = None;
    let refusal = d.stands_inner(&context()).await.unwrap_err();
    assert_eq!(refusal.reason, COMPLETION_NO_VAULT);
}
