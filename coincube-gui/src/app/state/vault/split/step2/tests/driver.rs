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
