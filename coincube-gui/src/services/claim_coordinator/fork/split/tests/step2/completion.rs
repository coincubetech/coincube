//! Split (#568 B5a) completion tests: the evidence, the digest-only
//! `split_from` record, the gated descriptor deletion and the post-completion
//! reconciliation. Step 2 is submitted through the coordinator over the
//! Connect route, then the journal is reopened by the reconciler, as a
//! restart does; the synthetic view says where step 2 is on BTCB2.
use super::*;
use crate::{
    app::settings::{
        update_settings_file, CubeSettings, Settings, SplitFromRecord, VaultIdentity,
        SETTINGS_FILE_NAME,
    },
    dir::CoincubeDirectory,
    services::{
        claim_coordinator::fork::split::step2::{
            CompletionTarget, SplitCompletionEvidence, SplitCompletionReconciliation,
            SplitStep2Reconciler,
        },
        coincube::network_anchor::NetworkAnchorStatus,
    },
};

/// Step 2's BTCB2 block at exactly [`MIN_CONFIRMATIONS`] below the fork tip.
const STEP2_HEIGHT: u64 = FORK_TIP + 1 - MIN_CONFIRMATIONS;
fn confirmed(txid: Txid, height: u64) -> TransactionObservation {
    // The synthetic indexer answers `hash(2)` for every BTCB2 height.
    TransactionObservation::Confirmed {
        txid,
        block: BlockRef {
            height,
            hash: hash(2),
        },
    }
}

/// Step 2 reviewed and submitted through the coordinator, which is then
/// dropped: a journal with a recorded step-2 submission, and its txid. The
/// review's deadline comes from [`wide_policy`], so load cannot expire it.
async fn submitted() -> (Harness, Txid) {
    let s = Step2::with_policy(wide_policy()).await;
    let signed_tx = s.signed_tx();
    let (transport, _server, _, _) = transport(&s.h, &signed_tx, true).await;
    let (h, mut coordinator) = finish(s, transport);
    let review = coordinator.prepare_review(&context()).await.unwrap();
    coordinator
        .confirm_and_submit(review, &context())
        .await
        .unwrap();
    drop(coordinator);
    (h, signed_tx.compute_txid())
}
/// The journal reopened by the reconciler over `services`, under
/// [`wide_policy`]: its evidence and its reconcile's clearing deadline
/// outlast a loaded run, so persisting, forgetting and clearing within one
/// check never lapse.
fn reopen(h: &Harness, services: Box<dyn SplitForkServices>) -> SplitStep2Reconciler {
    reopen_with(h, services, wide_policy())
}
/// [`reopen`] under `policy`.
fn reopen_with(
    h: &Harness,
    services: Box<dyn SplitForkServices>,
    policy: CheckPolicy,
) -> SplitStep2Reconciler {
    SplitStep2Reconciler::open(
        &h.temp.0,
        TARGET.into(),
        h.step1.source().digest(),
        context(),
        h.sender.subscribe(),
        services,
        policy,
    )
    .unwrap()
}
/// A settings root whose BTCB2 network directory holds the target Cube with
/// the harness Vault, next to an unrelated Cube of another Vault.
async fn settings_root(h: &Harness) -> (CoincubeDirectory, CompletionTarget) {
    let root = CoincubeDirectory::new(h.temp.0.join("settings-root"));
    let cube = CubeSettings::new_with_raw_id(TARGET.into(), TARGET.into(), ChainId::BitcoinBlake2b)
        .with_vault(VaultIdentity::generate(&vault()));
    let target = CompletionTarget::of(&cube).unwrap();
    let other = CubeSettings::new_with_raw_id(
        "other-cube".into(),
        "other-cube".into(),
        ChainId::BitcoinBlake2b,
    )
    .with_vault(VaultIdentity::generate(&other_vault()));
    update_settings_file(&root.network_directory(ChainId::BitcoinBlake2b), |mut s| {
        s.cubes.push(cube);
        s.cubes.push(other);
        Some(s)
    })
    .await
    .unwrap();
    (root, target)
}
fn settings_path(root: &CoincubeDirectory) -> PathBuf {
    root.network_directory(ChainId::BitcoinBlake2b)
        .path()
        .join(SETTINGS_FILE_NAME)
}
fn btcb2_settings(root: &CoincubeDirectory) -> Settings {
    Settings::from_file(&root.network_directory(ChainId::BitcoinBlake2b)).unwrap()
}
/// The target Cube's completed Splits.
fn split_from(root: &CoincubeDirectory) -> Vec<SplitFromRecord> {
    btcb2_settings(root)
        .cubes
        .iter()
        .find(|cube| cube.id == TARGET)
        .unwrap()
        .split_from
        .clone()
}
fn record(h: &Harness, txid: Txid, height: u64) -> SplitFromRecord {
    SplitFromRecord {
        descriptor_digest: h.step1.source().digest(),
        completed_height: height,
        step2_txid: txid,
    }
}
async fn minted(reconciler: &mut SplitStep2Reconciler) -> SplitCompletionEvidence {
    reconciler
        .check_completion(&context())
        .await
        .unwrap()
        .unwrap()
}
/// A Cube's Vault fingerprint rewritten in the settings file.
async fn rewrite_fingerprint(root: &CoincubeDirectory, id: &str, fingerprint: Option<&str>) {
    let fingerprint = fingerprint.map(str::to_owned);
    update_settings_file(&root.network_directory(ChainId::BitcoinBlake2b), |mut s| {
        s.cubes
            .iter_mut()
            .find(|cube| cube.id == id)
            .unwrap()
            .vault_fingerprint = fingerprint;
        Some(s)
    })
    .await
    .unwrap();
}

/// The harness view whose BTCB2 reads of `txid` answer `after` once
/// `flip_after` of them were served: a view that changes between two
/// collections, not within one (each collection reads step 2 twice).
#[derive(Clone)]
struct Flipping {
    inner: Chains,
    txid: Txid,
    reads: Arc<AtomicUsize>,
    flip_after: usize,
    after: TransactionObservation,
}
#[async_trait]
impl ObservationSource for Flipping {
    fn now(&self) -> i64 {
        self.inner.now()
    }
    async fn anchor(&self, chain: ChainId) -> Result<NetworkAnchorStatus, FailureKind> {
        self.inner.anchor(chain).await
    }
    async fn tip(&self, chain: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
        self.inner.tip(chain).await
    }
    async fn transaction(
        &self,
        chain: ChainId,
        txid: Txid,
    ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
        if chain != ChainId::Bitcoin
            && txid == self.txid
            && self.reads.fetch_add(1, Ordering::SeqCst) >= self.flip_after
        {
            return Chains::read(chain, self.after);
        }
        self.inner.transaction(chain, txid).await
    }
    async fn hash_at_height(
        &self,
        chain: ChainId,
        height: u64,
    ) -> Result<FreshRead<BlockHash>, FailureKind> {
        self.inner.hash_at_height(chain, height).await
    }
}
#[async_trait]
impl SplitForkServices for Flipping {
    fn source(&self) -> &dyn ObservationSource {
        self
    }
    fn origin(&self) -> &str {
        SplitForkServices::origin(&self.inner)
    }
    async fn btcb2_unspent(&self, address: &str) -> Result<FreshRead<Vec<OutPoint>>, FailureKind> {
        self.inner.btcb2_unspent(address).await
    }
    async fn address_used(
        &self,
        chain: ChainId,
        address: &str,
    ) -> Result<FreshRead<bool>, FailureKind> {
        self.inner.address_used(chain, address).await
    }
}

/// D14 and the step-1 gate: no evidence while step 2 is absent, unconfirmed
/// or below six confirmations on BTCB2, nor while step 1 is below six on
/// Bitcoin, seen on BTCB2, or re-mined out of its recorded block; with all
/// of it, evidence bound to the journal's identity and step 2's block. The
/// check records the sighting (which ends any resend) and nothing else.
#[tokio::test(flavor = "multi_thread")]
async fn split_completion_needs_step2_at_depth_and_step1_six_deep() {
    assert_eq!(MIN_CONFIRMATIONS, 6);
    let (h, txid) = submitted().await;
    let mut reconciler = reopen(&h, Box::new(h.chains.clone()));
    let none = |result: Result<Option<SplitCompletionEvidence>, Error>, case: &str| {
        assert!(result.unwrap().is_none(), "{}", case);
    };
    none(reconciler.check_completion(&context()).await, "absent");
    assert_eq!(h.temp.journal()["split"].get("step2_observed"), None);
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, TransactionObservation::Unconfirmed { txid })]);
    none(reconciler.check_completion(&context()).await, "unconfirmed");
    assert_eq!(h.temp.journal()["split"]["step2_observed"], true);
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, confirmed(txid, STEP2_HEIGHT + 1))]);
    none(reconciler.check_completion(&context()).await, "five deep");
    h.chains.edit(|view| {
        view.on_btcb2 = vec![(txid, confirmed(txid, STEP2_HEIGHT))];
        view.set_depth(5);
    });
    none(
        reconciler.check_completion(&context()).await,
        "step 1 five deep",
    );
    h.chains.edit(|view| {
        view.set_depth(6);
        view.on_fork = true;
    });
    none(
        reconciler.check_completion(&context()).await,
        "step 1 on BTCB2",
    );
    h.chains.edit(|view| view.on_fork = false);

    let evidence = reconciler
        .check_completion(&context())
        .await
        .unwrap()
        .expect("six deep on both chains");
    assert!(evidence.is_live());
    assert_eq!(evidence.txid(), txid);
    assert_eq!(evidence.block().height, STEP2_HEIGHT);
    assert_eq!(evidence.target_cube(), TARGET);
    assert_eq!(evidence.source_digest(), h.step1.source().digest());
    assert_eq!(evidence.fork_chain(), ChainId::BitcoinBlake2b);
    assert_eq!(evidence.record(), record(&h, txid, STEP2_HEIGHT));
    // A deeper tip still passes; a tip one block higher is the same block.
    h.chains.edit(|view| view.fork_tip = FORK_TIP + 10);
    assert_eq!(minted(&mut reconciler).await.block().height, STEP2_HEIGHT);
    // Nothing about completion is journaled by the check; the descriptors
    // stay until an explicit forget.
    let journal = h.temp.journal();
    assert_eq!(journal["phase"], "Tracking");
    assert!(journal["split"]["descriptors"].is_object());
    assert_eq!(journal["split"]["step2_observed"], true);
    assert!(journal.get("split_from").is_none());

    // Step 1 re-mined into another block: refused until the step-1
    // coordinator's reconfirmation review (its recorded block differs).
    h.chains.edit(|view| {
        view.step1_block = Some(BlockRef {
            height: 100,
            hash: hash(9),
        });
        view.set_depth(6);
    });
    none(
        reconciler.check_completion(&context()).await,
        "step 1 re-mined",
    );
}

/// The recheck: step 2 re-mined, or gone, between the reconcile and the
/// evidence collection refuses as a changed review and mints nothing; a
/// view that changes within one collection fails that collection; a stable
/// view then passes.
#[tokio::test(flavor = "multi_thread")]
async fn split_completion_is_refused_by_a_reorg() {
    let (h, txid) = submitted().await;
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, confirmed(txid, STEP2_HEIGHT))]);
    for (case, after) in [
        ("re-mined", confirmed(txid, STEP2_HEIGHT - 1)),
        ("gone", TransactionObservation::Absent),
        (
            "back in a mempool",
            TransactionObservation::Unconfirmed { txid },
        ),
    ] {
        let flipping = Flipping {
            inner: h.chains.clone(),
            txid,
            reads: Arc::new(AtomicUsize::new(0)),
            flip_after: 2,
            after,
        };
        let mut reconciler = reopen(&h, Box::new(flipping.clone()));
        assert!(
            matches!(
                reconciler.check_completion(&context()).await,
                Err(Error::ChangedReview)
            ),
            "{}",
            case
        );
        // Two reads of step 2 per collection: the reconcile saw the stable
        // block, the recheck the change.
        assert_eq!(flipping.reads.load(Ordering::SeqCst), 4, "{}", case);
    }
    // Within one collection: the collection itself refuses.
    h.chains
        .edit(|view| view.on_btcb2_once = Some((txid, TransactionObservation::Absent)));
    let mut reconciler = reopen(&h, Box::new(h.chains.clone()));
    assert!(matches!(
        reconciler.check_completion(&context()).await,
        Err(Error::Observation(claim_observation::Failure {
            kind: FailureKind::Changed,
            ..
        }))
    ));
    // Stable: minted.
    assert_eq!(minted(&mut reconciler).await.block().height, STEP2_HEIGHT);
}

/// Acceptance: the record is digest-only and the descriptors are deleted
/// only after it is written (D18). Forgetting before the record, or after a
/// refused write, is refused and keeps the descriptors; after the record,
/// the journal keeps the signed step 2 and its submission but no source,
/// neither file names a key or a descriptor, and the reconciler still
/// reconciles the forgotten journal.
#[tokio::test(flavor = "multi_thread")]
async fn split_completion_persists_digest_only_then_forgets() {
    let (h, txid) = submitted().await;
    let (root, target) = settings_root(&h).await;
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, confirmed(txid, STEP2_HEIGHT))]);
    let mut reconciler = reopen(&h, Box::new(h.chains.clone()));
    let before = std::fs::read(settings_path(&root)).unwrap();
    // The journal names the descriptors before completion: the grep below
    // is meaningful.
    let journal_text = std::fs::read_to_string(h.temp.0.join("intent.json")).unwrap();
    assert!(journal_text.contains("pkh(") && journal_text.contains("xpub"));

    // Forget before the record is written: refused, descriptors kept.
    let fresh = minted(&mut reconciler).await;
    assert!(matches!(
        fresh.forget(&mut reconciler, &context()),
        Err(Error::CompletionPersistence(_))
    ));
    assert!(h.temp.journal()["split"]["descriptors"].is_object());
    // A refused write (the Cube's Vault changed) records nothing and
    // permits no forgetting either.
    rewrite_fingerprint(&root, TARGET, Some("changed")).await;
    let changed = std::fs::read(settings_path(&root)).unwrap();
    let fresh = minted(&mut reconciler).await;
    assert!(fresh.persist(&root, &target).await.is_err());
    assert_eq!(std::fs::read(settings_path(&root)).unwrap(), changed);
    assert!(matches!(
        fresh.forget(&mut reconciler, &context()),
        Err(Error::CompletionPersistence(_))
    ));
    assert!(h.temp.journal()["split"]["descriptors"].is_object());
    rewrite_fingerprint(&root, TARGET, Some(&target.vault_fingerprint)).await;
    assert_eq!(std::fs::read(settings_path(&root)).unwrap(), before);

    // Record, then forget.
    let evidence = minted(&mut reconciler).await;
    evidence.persist(&root, &target).await.unwrap();
    assert_eq!(split_from(&root), vec![record(&h, txid, STEP2_HEIGHT)]);
    assert!(h.temp.journal()["split"]["descriptors"].is_object());
    evidence.forget(&mut reconciler, &context()).unwrap();
    let journal = h.temp.journal();
    assert!(journal["split"].get("descriptors").is_none());
    assert_eq!(
        journal["split"]["source_digest"],
        h.step1.source().digest().to_string()
    );
    assert_eq!(journal["fork_submission"]["txid"], txid.to_string());
    assert!(journal["split"]["step2_transaction"].is_object());
    assert!(journal["bitcoin_transaction"].is_object());
    assert_eq!(journal["phase"], "Tracking");
    // Only the BTCB2 directory was written.
    assert!(!root
        .network_directory(ChainId::Bitcoin)
        .path()
        .join(SETTINGS_FILE_NAME)
        .exists());
    // Digest only: neither file names a key or a descriptor.
    for (file, bytes) in [
        ("settings", std::fs::read(settings_path(&root)).unwrap()),
        (
            "journal",
            std::fs::read(h.temp.0.join("intent.json")).unwrap(),
        ),
    ] {
        let text = String::from_utf8(bytes).unwrap();
        for needle in ["xpub", "tpub", "wsh(", "wpkh(", "pkh("] {
            assert!(!text.contains(needle), "{} names {}", file, needle);
        }
    }
    // The forgotten journal still reconciles, and reopens with no source.
    let seen = reconciler.reconcile_sweep(&context()).await.unwrap().step2;
    assert_eq!(seen, confirmed(txid, STEP2_HEIGHT));
    drop(reconciler);
    let controller = Controller::reopen_settling_blocking(
        &h.temp.0,
        &claim_workflow::split_identity(TARGET.into(), h.step1.source().digest()),
        context(),
    )
    .unwrap();
    let recorded = controller.recorded_split().unwrap().unwrap();
    assert!(recorded.source.is_none());
    assert_eq!(recorded.source_digest, h.step1.source().digest());
}

/// The evidence lapses with its deadline, is superseded by any later check
/// of the reconciler, and dies with a revocation, a generation change or the
/// reconciler itself; a lapsed evidence writes nothing and forgets nothing.
#[tokio::test(flavor = "multi_thread")]
async fn split_completion_evidence_dies_with_the_session() {
    let (h, txid) = submitted().await;
    let (root, target) = settings_root(&h).await;
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, confirmed(txid, STEP2_HEIGHT))]);
    let before = std::fs::read(settings_path(&root)).unwrap();

    // The deadline is the collection budget (the harness's 2 s here) at
    // most. Only this reconciler keeps the short budget: every assertion on
    // its evidence is a lapse, which load can only hasten.
    let mut short = reopen_with(&h, Box::new(h.chains.clone()), policy());
    let evidence = minted(&mut short).await;
    let deadline = evidence.not_after();
    assert!(deadline <= Instant::now() + Duration::from_secs(2));
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
    assert!(!evidence.is_live());
    assert!(evidence.persist(&root, &target).await.is_err());
    assert!(matches!(
        evidence.forget(&mut short, &context()),
        Err(Error::ExpiredEvidence)
    ));
    drop(short);

    // Superseded by a later check, a reconcile or a completion check.
    let mut reconciler = reopen(&h, Box::new(h.chains.clone()));
    let earlier = minted(&mut reconciler).await;
    reconciler.reconcile_sweep(&context()).await.unwrap();
    assert!(!earlier.is_live());
    let earlier = minted(&mut reconciler).await;
    let later = minted(&mut reconciler).await;
    assert!(!earlier.is_live() && later.is_live());
    assert!(earlier.persist(&root, &target).await.is_err());

    // Revocation (logout revokes synchronously).
    let evidence = minted(&mut reconciler).await;
    reconciler.revoker().revoke();
    assert!(!evidence.is_live());
    assert!(evidence.persist(&root, &target).await.is_err());
    assert!(matches!(
        reconciler.check_completion(&context()).await,
        Err(Error::Revoked)
    ));
    drop(reconciler);

    // A generation change.
    let mut reconciler = reopen(&h, Box::new(h.chains.clone()));
    let evidence = minted(&mut reconciler).await;
    h.sender.send(8).unwrap();
    assert!(!evidence.is_live());
    assert!(evidence.persist(&root, &target).await.is_err());
    drop(reconciler);
    h.sender.send(7).unwrap();

    // Dropping the reconciler (Cube closed).
    let mut reconciler = reopen(&h, Box::new(h.chains.clone()));
    let evidence = minted(&mut reconciler).await;
    drop(reconciler);
    assert!(!evidence.is_live());
    assert!(evidence.persist(&root, &target).await.is_err());
    // Another reconciler's evidence does not forget on this one.
    let mut first = reopen(&h, Box::new(h.chains.clone()));
    let evidence = minted(&mut first).await;
    evidence.persist(&root, &target).await.unwrap();
    drop(first);
    let mut second = reopen(&h, Box::new(h.chains.clone()));
    assert!(matches!(
        evidence.forget(&mut second, &context()),
        Err(Error::InvalidBinding)
    ));
    assert!(h.temp.journal()["split"]["descriptors"].is_object());
    // Nothing but that one live persist reached the file.
    assert_eq!(split_from(&root), vec![record(&h, txid, STEP2_HEIGHT)]);
    assert_ne!(std::fs::read(settings_path(&root)).unwrap(), before);
}

/// The record is written once however often the same completion persists;
/// another Split's record is kept beside it; a target that is not the
/// journal's Cube, a Cube whose Vault no longer matches, or a Cube missing
/// from the file refuses with the file unchanged, and the Bitcoin network
/// directory is never touched.
#[tokio::test(flavor = "multi_thread")]
async fn split_completion_persist_is_idempotent_and_refuses_a_mismatched_vault() {
    let (h, txid) = submitted().await;
    let (root, target) = settings_root(&h).await;
    let other = SplitFromRecord {
        descriptor_digest: sha256::Hash::hash(b"another source"),
        completed_height: 7,
        step2_txid: Txid::from_byte_array([7; 32]),
    };
    update_settings_file(&root.network_directory(ChainId::BitcoinBlake2b), |mut s| {
        s.cubes[0].split_from.push(other.clone());
        Some(s)
    })
    .await
    .unwrap();
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, confirmed(txid, STEP2_HEIGHT))]);
    let mut reconciler = reopen(&h, Box::new(h.chains.clone()));
    let evidence = minted(&mut reconciler).await;
    evidence.persist(&root, &target).await.unwrap();
    let written = std::fs::read(settings_path(&root)).unwrap();
    evidence.persist(&root, &target).await.unwrap();
    assert_eq!(std::fs::read(settings_path(&root)).unwrap(), written);
    let fresh = minted(&mut reconciler).await;
    fresh.persist(&root, &target).await.unwrap();
    assert_eq!(std::fs::read(settings_path(&root)).unwrap(), written);
    assert_eq!(
        split_from(&root),
        vec![other.clone(), record(&h, txid, STEP2_HEIGHT)]
    );
    // The unrelated Cube is untouched.
    assert!(btcb2_settings(&root).cubes[1].split_from.is_empty());

    // Refusals, each with the file unchanged.
    let mut wrong_cube = target.clone();
    wrong_cube.cube_id = "other-cube".into();
    let mut wrong_fingerprint = target.clone();
    wrong_fingerprint.vault_fingerprint = "00000000".into();
    let mut wrong_checksum = target.clone();
    wrong_checksum.vault_wallet_id.descriptor_checksum = "zzzzzzzz".into();
    let mut missing = target.clone();
    missing.cube_id = "missing-cube".into();
    for (case, wrong) in [
        ("another Cube", wrong_cube),
        ("changed fingerprint", wrong_fingerprint),
        ("changed checksum", wrong_checksum),
        ("missing Cube", missing),
    ] {
        let fresh = minted(&mut reconciler).await;
        assert!(fresh.persist(&root, &wrong).await.is_err(), "{}", case);
        assert_eq!(
            std::fs::read(settings_path(&root)).unwrap(),
            written,
            "{}",
            case
        );
    }
    // The file's own Cube changed underneath a live evidence.
    rewrite_fingerprint(&root, TARGET, None).await;
    let changed = std::fs::read(settings_path(&root)).unwrap();
    let fresh = minted(&mut reconciler).await;
    assert!(fresh
        .persist(&root, &target)
        .await
        .unwrap_err()
        .to_string()
        .contains("no longer matches"));
    assert_eq!(std::fs::read(settings_path(&root)).unwrap(), changed);
    assert!(!root
        .network_directory(ChainId::Bitcoin)
        .path()
        .join(SETTINGS_FILE_NAME)
        .exists());
    // No settings file at all: refused, none created.
    let empty = CoincubeDirectory::new(h.temp.0.join("no-settings"));
    let fresh = minted(&mut reconciler).await;
    assert!(fresh.persist(&empty, &target).await.is_err());
    assert!(!settings_path(&empty).exists());
}

/// D17: after completion, a reconcile that finds step 2 still in its block
/// and step 1 deep keeps the record; step 2 absent, unconfirmed or re-mined
/// into another block, or step 1 below six (also behind an RDTS margin
/// refusal), clears it while the descriptors stay forgotten and another
/// Split's record stays. A failing collection, a changing view or a target
/// that is not the journal's Cube leaves the file alone.
#[tokio::test(flavor = "multi_thread")]
async fn split_completion_marker_is_cleared_when_step2_leaves_its_block() {
    let (h, txid) = submitted().await;
    let (root, target) = settings_root(&h).await;
    let other = SplitFromRecord {
        descriptor_digest: sha256::Hash::hash(b"another source"),
        completed_height: 7,
        step2_txid: Txid::from_byte_array([7; 32]),
    };
    update_settings_file(&root.network_directory(ChainId::BitcoinBlake2b), |mut s| {
        s.cubes[0].split_from.push(other.clone());
        Some(s)
    })
    .await
    .unwrap();
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, confirmed(txid, STEP2_HEIGHT))]);
    let mut reconciler = reopen(&h, Box::new(h.chains.clone()));
    let evidence = minted(&mut reconciler).await;
    evidence.persist(&root, &target).await.unwrap();
    evidence.forget(&mut reconciler, &context()).unwrap();
    let completed = || vec![other.clone(), record(&h, txid, STEP2_HEIGHT)];
    assert_eq!(split_from(&root), completed());
    let standing = |result: Result<SplitCompletionReconciliation, Error>| {
        assert!(
            matches!(
                result,
                Ok(SplitCompletionReconciliation::Standing {
                    status: Status::Observation(Assessment::ObservationsEligibleForPreflight),
                    ..
                })
            ),
            "{:?}",
            result
        );
    };
    let lost = |result: Result<SplitCompletionReconciliation, Error>, cleared: bool| {
        assert!(
            matches!(
                result,
                Ok(SplitCompletionReconciliation::Lost { cleared: c, .. }) if c == cleared
            ),
            "{:?}",
            result
        );
    };

    // Standing.
    standing(
        reconciler
            .reconcile_split_completion(&context(), &root, &target)
            .await,
    );
    assert_eq!(split_from(&root), completed());
    // A failing collection leaves the record.
    h.chains.edit(|view| view.read_age = 3_600);
    assert!(reconciler
        .reconcile_split_completion(&context(), &root, &target)
        .await
        .is_err());
    h.chains.edit(|view| view.read_age = 0);
    assert_eq!(split_from(&root), completed());
    // A target that is not the journal's Cube.
    let mut wrong = target.clone();
    wrong.cube_id = "other-cube".into();
    assert!(matches!(
        reconciler
            .reconcile_split_completion(&context(), &root, &wrong)
            .await,
        Err(Error::InvalidBinding)
    ));
    assert_eq!(split_from(&root), completed());

    // Step 1 below six: cleared; the other record stays; recorded again
    // from fresh evidence once deep.
    h.chains.edit(|view| view.set_depth(5));
    lost(
        reconciler
            .reconcile_split_completion(&context(), &root, &target)
            .await,
        true,
    );
    assert_eq!(split_from(&root), vec![other.clone()]);
    assert!(h.temp.journal()["split"].get("descriptors").is_none());
    lost(
        reconciler
            .reconcile_split_completion(&context(), &root, &target)
            .await,
        false,
    );
    h.chains.edit(|view| view.set_depth(6));
    minted(&mut reconciler)
        .await
        .persist(&root, &target)
        .await
        .unwrap();
    assert_eq!(split_from(&root), completed());
    // Step 1 below six behind an RDTS margin refusal: the loss is read from
    // the chain observations, not the journal's assessment.
    h.chains.edit(|view| {
        view.rdts_expiry = MTP + 600;
        view.set_depth(5);
    });
    lost(
        reconciler
            .reconcile_split_completion(&context(), &root, &target)
            .await,
        true,
    );
    h.chains.edit(|view| {
        view.rdts_expiry = 20_000;
        view.set_depth(6);
    });
    minted(&mut reconciler)
        .await
        .persist(&root, &target)
        .await
        .unwrap();
    assert_eq!(split_from(&root), completed());

    // Step 2 re-mined into another block: cleared, then recorded at the new
    // height.
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, confirmed(txid, STEP2_HEIGHT - 1))]);
    lost(
        reconciler
            .reconcile_split_completion(&context(), &root, &target)
            .await,
        true,
    );
    assert_eq!(split_from(&root), vec![other.clone()]);
    minted(&mut reconciler)
        .await
        .persist(&root, &target)
        .await
        .unwrap();
    assert_eq!(
        split_from(&root),
        vec![other.clone(), record(&h, txid, STEP2_HEIGHT - 1)]
    );
    standing(
        reconciler
            .reconcile_split_completion(&context(), &root, &target)
            .await,
    );

    // Step 2 unconfirmed, then absent: cleared.
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, TransactionObservation::Unconfirmed { txid })]);
    lost(
        reconciler
            .reconcile_split_completion(&context(), &root, &target)
            .await,
        true,
    );
    assert_eq!(split_from(&root), vec![other.clone()]);
    h.chains.edit(|view| view.on_btcb2.clear());
    lost(
        reconciler
            .reconcile_split_completion(&context(), &root, &target)
            .await,
        false,
    );
    // A view that changes between the reconcile and the recheck leaves the
    // record alone: back at the recorded height, re-mined for the recheck.
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, confirmed(txid, STEP2_HEIGHT))]);
    minted(&mut reconciler)
        .await
        .persist(&root, &target)
        .await
        .unwrap();
    drop(reconciler);
    let flipping = Flipping {
        inner: h.chains.clone(),
        txid,
        reads: Arc::new(AtomicUsize::new(0)),
        flip_after: 2,
        after: confirmed(txid, STEP2_HEIGHT - 1),
    };
    let mut reconciler = reopen(&h, Box::new(flipping));
    assert!(matches!(
        reconciler
            .reconcile_split_completion(&context(), &root, &target)
            .await,
        Err(Error::ChangedReview)
    ));
    assert_eq!(split_from(&root), completed());
    // The descriptors were never restored.
    assert!(h.temp.journal()["split"].get("descriptors").is_none());
    // The unrelated Cube is untouched throughout.
    assert!(btcb2_settings(&root).cubes[1].split_from.is_empty());
}

/// #662 Reviewer-662d (B5c-2): a completion stands only on the Cube's record
/// of it. An earlier recheck cleared the record (and then expired, leaving
/// the panel in Completed); step 2 is back in its block and step 1 six deep,
/// yet with no record a later recheck is a loss with nothing to clear, never
/// Standing, so "complete" is never shown for a split this Cube no longer
/// records. Nothing is written; a fresh completion records it again.
#[tokio::test(flavor = "multi_thread")]
async fn split_completion_without_its_record_is_lost_not_standing() {
    let (h, txid) = submitted().await;
    let (root, target) = settings_root(&h).await;
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, confirmed(txid, STEP2_HEIGHT))]);
    let mut reconciler = reopen(&h, Box::new(h.chains.clone()));
    let evidence = minted(&mut reconciler).await;
    evidence.persist(&root, &target).await.unwrap();
    evidence.forget(&mut reconciler, &context()).unwrap();
    assert_eq!(split_from(&root), vec![record(&h, txid, STEP2_HEIGHT)]);
    // The earlier recheck: step 2 left its block, the record is cleared.
    h.chains.edit(|view| view.on_btcb2.clear());
    assert!(matches!(
        reconciler
            .reconcile_split_completion(&context(), &root, &target)
            .await,
        Ok(SplitCompletionReconciliation::Lost { cleared: true, .. })
    ));
    assert!(split_from(&root).is_empty());
    // Step 2 back in the very block, step 1 still six deep: no record, so
    // lost, and the settings file is not written.
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, confirmed(txid, STEP2_HEIGHT))]);
    let before = std::fs::read(settings_path(&root)).unwrap();
    let result = reconciler
        .reconcile_split_completion(&context(), &root, &target)
        .await;
    assert!(
        matches!(
            result,
            Ok(SplitCompletionReconciliation::Lost {
                status: Status::Observation(Assessment::ObservationsEligibleForPreflight),
                cleared: false,
                ..
            })
        ),
        "{:?}",
        result
    );
    assert_eq!(std::fs::read(settings_path(&root)).unwrap(), before);
    // Recorded again from fresh evidence, it stands again.
    minted(&mut reconciler)
        .await
        .persist(&root, &target)
        .await
        .unwrap();
    assert_eq!(split_from(&root), vec![record(&h, txid, STEP2_HEIGHT)]);
    assert!(matches!(
        reconciler
            .reconcile_split_completion(&context(), &root, &target)
            .await,
        Ok(SplitCompletionReconciliation::Standing { .. })
    ));
    // The descriptors were never restored.
    assert!(h.temp.journal()["split"].get("descriptors").is_none());
}

/// #645 P3-2: the cleared record is keyed by the source digest *and* step
/// 2's txid. A record of the same source with another step 2 stays when
/// this one's completion is lost (and is not taken for this one's height).
#[tokio::test(flavor = "multi_thread")]
async fn split_completion_loss_clears_only_its_own_step2_record() {
    let (h, txid) = submitted().await;
    let (root, target) = settings_root(&h).await;
    let same_source = SplitFromRecord {
        descriptor_digest: h.step1.source().digest(),
        completed_height: 7,
        step2_txid: Txid::from_byte_array([7; 32]),
    };
    update_settings_file(&root.network_directory(ChainId::BitcoinBlake2b), |mut s| {
        s.cubes[0].split_from.push(same_source.clone());
        Some(s)
    })
    .await
    .unwrap();
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, confirmed(txid, STEP2_HEIGHT))]);
    let mut reconciler = reopen(&h, Box::new(h.chains.clone()));
    minted(&mut reconciler)
        .await
        .persist(&root, &target)
        .await
        .unwrap();
    assert_eq!(
        split_from(&root),
        vec![same_source.clone(), record(&h, txid, STEP2_HEIGHT)]
    );
    // Standing: the other record's height is not this one's.
    assert!(matches!(
        reconciler
            .reconcile_split_completion(&context(), &root, &target)
            .await,
        Ok(SplitCompletionReconciliation::Standing { .. })
    ));
    // Step 2 gone: only its own record is cleared.
    h.chains.edit(|view| view.on_btcb2.clear());
    assert!(matches!(
        reconciler
            .reconcile_split_completion(&context(), &root, &target)
            .await,
        Ok(SplitCompletionReconciliation::Lost { cleared: true, .. })
    ));
    assert_eq!(split_from(&root), vec![same_source]);
}

/// #645 P3-3: a target naming another Cube whose settings hold the very
/// same Vault (fingerprint and checksum) is refused by the journal's own
/// Cube check, before and under the writer lock, with the file unchanged;
/// the Vault match alone would have admitted it.
#[tokio::test(flavor = "multi_thread")]
async fn split_completion_persist_refuses_the_same_vault_in_another_cube() {
    let (h, txid) = submitted().await;
    let (root, target) = settings_root(&h).await;
    let twin = CubeSettings::new_with_raw_id(
        "twin-cube".into(),
        "twin-cube".into(),
        ChainId::BitcoinBlake2b,
    )
    .with_vault(VaultIdentity::generate(&vault()));
    let twin_target = CompletionTarget::of(&twin).unwrap();
    assert_eq!(
        (
            &twin_target.vault_fingerprint,
            &twin_target.vault_wallet_id.descriptor_checksum
        ),
        (
            &target.vault_fingerprint,
            &target.vault_wallet_id.descriptor_checksum
        ),
        "the same Vault"
    );
    update_settings_file(&root.network_directory(ChainId::BitcoinBlake2b), |mut s| {
        s.cubes.push(twin);
        Some(s)
    })
    .await
    .unwrap();
    let before = std::fs::read(settings_path(&root)).unwrap();
    h.chains
        .edit(|view| view.on_btcb2 = vec![(txid, confirmed(txid, STEP2_HEIGHT))]);
    let mut reconciler = reopen(&h, Box::new(h.chains.clone()));
    let evidence = minted(&mut reconciler).await;
    let refused = evidence.persist(&root, &twin_target).await.unwrap_err();
    assert!(
        refused.to_string().contains("not the journal's"),
        "{}",
        refused
    );
    assert_eq!(std::fs::read(settings_path(&root)).unwrap(), before);
    // The journal's own Cube is recorded by the same evidence.
    evidence.persist(&root, &target).await.unwrap();
    assert_eq!(split_from(&root), vec![record(&h, txid, STEP2_HEIGHT)]);
    assert!(btcb2_settings(&root)
        .cubes
        .iter()
        .find(|cube| cube.id == "twin-cube")
        .unwrap()
        .split_from
        .is_empty());
}
