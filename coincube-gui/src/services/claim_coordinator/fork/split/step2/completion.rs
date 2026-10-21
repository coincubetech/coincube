//! Split (#568 B5a) completion: short-lived evidence that the recorded step 2
//! is confirmed on BTCB2 while step 1 is still six deep on Bitcoin, the
//! digest-only `split_from` record it persists on the target Cube, the
//! descriptor deletion that record then permits (owner decision P2), and the
//! reconciliation that clears the record when the chains take the completion
//! back.
//!
//! Owner defaults, recorded on #568 as D14–D18: step 2 needs
//! [`MIN_CONFIRMATIONS`] on BTCB2 (D14); the record holds the source digest,
//! the BTCB2 height and step 2's txid, never a descriptor, and is history for
//! the Split panel only (D16); a post-completion reorg clears the record while
//! the descriptors stay forgotten (D17); the record is written before the
//! descriptors are forgotten, under one live evidence (D18). Refusing a second
//! Split of a recorded source (D15) belongs to the go-live (B5c), which reads
//! `CubeSettings::split_from`; nothing here refuses one.
//!
//! [`SplitStep2Reconciler::check_completion`] mirrors Claim's
//! `Coordinator::check_completion`. It reconciles the recorded step 2 (the
//! ordinary reconcile, applied to the journal, which also records the
//! sighting that ends any resend), then collects the same sweep once more,
//! fresh; the recheck must see step 2 in the same block, with step 1 confirmed
//! at depth in its recorded Bitcoin block and absent from BTCB2 at the
//! recheck's tips. Only then is a [`SplitCompletionEvidence`] minted: no
//! Clone, no serialization, no public constructor; bound to this reconciler
//! (dropping it kills the evidence), to the check (any later check supersedes
//! it), to the session generation and revoker, and to a short monotonic
//! deadline. It is historical metadata and never spending, signing or replay
//! authority.
//!
//! The coordinator that submitted step 2 has no completion check: once a
//! reconcile has seen step 2 on BTCB2 the coordinator can only reconcile
//! anyway (its resend is withdrawn), so completion opens the journal through
//! this reconciler, as the restart after a submission already does.
use super::*;
use crate::{
    app::settings::{
        bitcoin_import::ImportedClaimPair, update_settings_file_checked, CubeSettings,
        ImportedWalletAttachment, Settings, SettingsError, SplitFromRecord, WalletId,
        SETTINGS_FILE_NAME,
    },
    dir::CoincubeDirectory,
};
use coincube_core::claim::{BlockRef, ForkTransactionPresence, MIN_CONFIRMATIONS};
use std::sync::{atomic::AtomicBool, Weak};

/// The target Cube and wallet a completion is recorded on. Native targets
/// retain their Vault identity; imported targets additionally bind the exact
/// paired attachment, source operation and descriptor digest. Writers recheck
/// that identity under the target settings lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionTarget {
    pub cube_id: String,
    /// The native Vault's id, or the paired imported wallet's distinct id.
    pub vault_wallet_id: WalletId,
    /// The native Vault fingerprint, or the imported Cube's unlock identity.
    pub vault_fingerprint: String,
    imported: Option<Box<ImportedCompletionTarget>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImportedCompletionTarget {
    source: ImportedClaimPair,
    wallet: ImportedWalletAttachment,
    operation_id: uuid::Uuid,
    source_digest: sha256::Hash,
}

impl CompletionTarget {
    /// The completion target of a native Vault or a valid dedicated imported
    /// pair. Unpaired imports and incomplete/conflicting records are refused.
    pub fn of(cube: &CubeSettings) -> Option<Self> {
        if let Some(source) = cube.imported_claim_source.as_deref() {
            crate::services::bitcoin_import::pairing::validate_target_record(cube).ok()?;
            let import = cube.bitcoin_import.as_ref()?;
            let wallet = import.active_attachment()?.clone();
            let descriptor = &source.source_wallet.descriptor;
            let source_digest = coincube_core::foreign_split::SplitSource::new(
                descriptor.receive_descriptor().clone(),
                Some(descriptor.change_descriptor().clone()),
            )
            .ok()?
            .digest();
            return Some(Self {
                cube_id: cube.id.clone(),
                vault_wallet_id: wallet.wallet_id.clone(),
                vault_fingerprint: cube.master_signer_fingerprint?.to_string(),
                imported: Some(Box::new(ImportedCompletionTarget {
                    source: source.clone(),
                    wallet,
                    operation_id: import.operation_id,
                    source_digest,
                })),
            });
        }
        Some(Self {
            cube_id: cube.id.clone(),
            vault_wallet_id: cube.vault_wallet_id.clone()?,
            vault_fingerprint: cube.vault_fingerprint.clone()?,
            imported: None,
        })
    }

    fn matching_cube<'a>(
        &self,
        root: &CoincubeDirectory,
        settings: &'a mut Settings,
        chain: ChainId,
        source_digest: sha256::Hash,
    ) -> Result<&'a mut CubeSettings, SettingsError> {
        let Some(imported) = &self.imported else {
            return matching_completion_cube(
                settings,
                chain,
                &self.cube_id,
                &self.vault_fingerprint,
                &self.vault_wallet_id.descriptor_checksum,
            );
        };
        if chain != ChainId::BitcoinBlake2b
            || source_digest != imported.source_digest
            || settings
                .cubes
                .iter()
                .filter(|cube| cube.id == self.cube_id)
                .count()
                != 1
        {
            return Err(SettingsError::Unexpected(
                "Imported Claim completion no longer matches its source or target Cube".into(),
            ));
        }
        let cube = settings
            .cubes
            .iter_mut()
            .find(|cube| cube.id == self.cube_id)
            .unwrap();
        if Self::of(cube).as_ref() != Some(self) {
            return Err(SettingsError::Unexpected(
                "Imported Claim completion no longer matches its paired wallet".into(),
            ));
        }
        crate::services::bitcoin_import::pairing::validate_pair(root, cube)
            .map_err(SettingsError::Unexpected)?;
        Ok(cube)
    }
}

/// Non-serializable, generation- and lifetime-bound evidence that the
/// recorded Split step 2 is currently confirmed at depth on BTCB2 and step 1
/// still has its required depth on Bitcoin. Only
/// [`SplitStep2Reconciler::check_completion`] constructs it.
pub struct SplitCompletionEvidence {
    /// The Split journal's target Cube (its identity's fork Cube).
    target_cube: String,
    /// The Split journal's source digest (D9).
    source_digest: sha256::Hash,
    fork_chain: ChainId,
    /// Step 2's confirming BTCB2 block at the check.
    block: BlockRef,
    /// The signed step 2's own txid, as recorded and observed.
    txid: Txid,
    /// Set once [`Self::persist`] wrote the record while still live (D18).
    persisted: AtomicBool,
    check_revoker: Revoker,
    lifetime: Weak<()>,
    generation: watch::Receiver<u64>,
    expected_generation: u64,
    revoker: Revoker,
    not_after: Instant,
}
impl std::fmt::Debug for SplitCompletionEvidence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SplitCompletionEvidence")
            .finish_non_exhaustive()
    }
}

/// What [`SplitStep2Reconciler::reconcile_split_completion`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitCompletionReconciliation {
    /// Step 2 is still in the block this Cube's record of it names and step
    /// 1 still has its depth: the record stands. Never without a record.
    Standing {
        status: Status,
        transaction: claim_observation::TransactionObservation,
    },
    /// Step 2 left its block (absent, unconfirmed or re-mined elsewhere),
    /// step 1 lost its depth, or this Cube holds no record of this split's
    /// step 2 (#662 Reviewer-662d: an earlier recheck cleared it and then
    /// expired). The matching record was cleared when `cleared`; otherwise
    /// there was none to clear. The descriptors stay forgotten.
    Lost {
        status: Status,
        transaction: claim_observation::TransactionObservation,
        cleared: bool,
    },
}

impl SplitCompletionEvidence {
    pub fn is_live(&self) -> bool {
        self.lifetime.upgrade().is_some()
            && !self.check_revoker.is_revoked()
            && !self.revoker.is_revoked()
            && self.generation.has_changed().is_ok()
            && *self.generation.borrow() == self.expected_generation
            && Instant::now() < self.not_after
    }
    pub fn target_cube(&self) -> &str {
        &self.target_cube
    }
    pub fn source_digest(&self) -> sha256::Hash {
        self.source_digest
    }
    pub fn fork_chain(&self) -> ChainId {
        self.fork_chain
    }
    pub fn block(&self) -> BlockRef {
        self.block
    }
    pub fn txid(&self) -> Txid {
        self.txid
    }
    /// The digest-only record this evidence persists.
    pub fn record(&self) -> SplitFromRecord {
        SplitFromRecord {
            descriptor_digest: self.source_digest,
            completed_height: self.block.height,
            step2_txid: self.txid,
        }
    }
    #[cfg(test)]
    pub(crate) fn not_after(&self) -> Instant {
        self.not_after
    }

    /// Record the completion on the target Cube, in the BTCB2 network
    /// directory only: the record is appended unless the same source digest
    /// and step-2 txid are already there, so a retry is a no-op. Refused,
    /// before the file is touched, unless the evidence is live and `target`
    /// is the journal's Cube with its recorded native or paired imported
    /// wallet; the same checks run again under the writer lock. A refusal after the write
    /// (the evidence lapsed meanwhile) is reported, and the evidence does not
    /// permit forgetting: check again.
    pub async fn persist(
        &self,
        root: &CoincubeDirectory,
        target: &CompletionTarget,
    ) -> Result<(), SettingsError> {
        if self.fork_chain != ChainId::BitcoinBlake2b {
            return Err(SettingsError::Unexpected(
                "Split completion is recorded on Bitcoin Blake2b only".into(),
            ));
        }
        let directory = root.network_directory(self.fork_chain);
        let mut settings = Settings::from_file(&directory)?;
        self.apply(root, &mut settings, target)?;
        update_settings_file_checked(&directory, |mut settings| {
            self.apply(root, &mut settings, target)?;
            Ok(Some(settings))
        })
        .await?;
        if !self.is_live() {
            return Err(SettingsError::Unexpected(
                "Split completion expired while saving; check both chains again".into(),
            ));
        }
        self.persisted.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn apply(
        &self,
        root: &CoincubeDirectory,
        settings: &mut Settings,
        target: &CompletionTarget,
    ) -> Result<(), SettingsError> {
        if !self.is_live() {
            return Err(SettingsError::Unexpected(
                "Split completion expired or changed; check both chains again".into(),
            ));
        }
        if target.cube_id != self.target_cube {
            return Err(SettingsError::Unexpected(
                "Split target Cube is not the journal's".into(),
            ));
        }
        let cube = target.matching_cube(root, settings, self.fork_chain, self.source_digest)?;
        let record = self.record();
        if !cube.split_from.iter().any(|existing| {
            existing.descriptor_digest == record.descriptor_digest
                && existing.step2_txid == record.step2_txid
        }) {
            cube.split_from.push(record);
        }
        Ok(())
    }

    /// Delete the foreign descriptors from the Split journal (P2), which
    /// ends every later rebuild of step 1 or step 2 from it. Permitted only
    /// by this evidence, while it is live, on the reconciler that minted it,
    /// and after [`Self::persist`] recorded the completion (D18): the record
    /// is what a later session has instead of the descriptors. Consumes the
    /// evidence; a refusal needs a fresh check.
    pub fn forget(
        self,
        reconciler: &mut SplitStep2Reconciler,
        context: &Context,
    ) -> Result<(), Error> {
        if !Weak::ptr_eq(&self.lifetime, &Arc::downgrade(&reconciler.lifetime)) {
            return Err(Error::InvalidBinding);
        }
        if !self.is_live() {
            return Err(Error::ExpiredEvidence);
        }
        if !self.persisted.load(Ordering::SeqCst) {
            return Err(Error::CompletionPersistence(
                "the Split completion is not recorded in the target Cube's settings yet".into(),
            ));
        }
        reconciler.current(context)?;
        let identity = reconciler.controller.identity();
        if reconciler.recorded_txid()? != self.txid
            || identity.descriptor_digest != self.source_digest
            || identity.fork_cube != self.target_cube
        {
            return Err(Error::InvalidBinding);
        }
        reconciler.controller.forget_split_descriptors(context)?;
        Ok(())
    }
}

/// Whether `block` has [`MIN_CONFIRMATIONS`] at `tip` (D14).
fn fork_depth_reached(tip: BlockRef, block: BlockRef) -> bool {
    tip.height
        .checked_sub(block.height)
        .and_then(|depth| depth.checked_add(1))
        .is_some_and(|depth| depth >= MIN_CONFIRMATIONS)
}

impl SplitStep2Reconciler {
    pub(super) fn current(&mut self, context: &Context) -> Result<(), Error> {
        if self.revoker.is_revoked()
            || context != &self.context
            || *self.generation.borrow() != context.generation
            || self.generation.has_changed().is_err()
        {
            self.revoker.revoke();
            self.controller.invalidate();
            return Err(Error::Revoked);
        }
        Ok(())
    }
    fn recorded_txid(&self) -> Result<Txid, Error> {
        self.controller
            .recorded_fork_submission()
            .map(|submission| submission.txid())
            .ok_or(Error::InvalidBinding)
    }
    /// A fresh collection of the recorded sweep after a reconcile that saw
    /// `seen`: the recheck. Both must see the same thing of step 2, so a
    /// step 2 reorged, re-mined or first seen between the two refuses as
    /// `ChangedReview`. A sighting the reconcile missed is still recorded
    /// (it ends any resend) before refusing.
    async fn recheck_sweep(
        &mut self,
        context: &Context,
        seen: claim_observation::TransactionObservation,
    ) -> Result<claim_observation::SweepObservation, Error> {
        let plan = self.controller.plan();
        let txid = self.recorded_txid()?;
        let recheck = claim_observation::collect_sweep(
            self.services.source(),
            &plan,
            txid,
            self.policy.observations,
            self.policy.collection_budget,
            CollectionContext {
                expected_generation: context.generation,
                generation: self.generation.clone(),
            },
        )
        .await
        .map_err(Error::Observation)?;
        self.current(context)?;
        if recheck.transaction() != seen {
            self.controller
                .record_split_step2_observed(context, recheck.transaction())?;
            return Err(Error::ChangedReview);
        }
        Ok(recheck)
    }

    /// Obtain short-lived evidence for recording the Split's completion; see
    /// the module documentation. `None` when the recorded step 2 is not
    /// confirmed at [`MIN_CONFIRMATIONS`] on BTCB2, or step 1 is not six
    /// deep in its recorded Bitcoin block, or step 1 is seen on BTCB2, or
    /// the reconcile found step 1 anything but [`Step1AfterStep2::Eligible`]
    /// (#568 S4: re-mined, in the mempool, missing, or a recorded conflict).
    /// Supersedes any earlier evidence of this reconciler, whatever the
    /// result.
    pub async fn check_completion(
        &mut self,
        context: &Context,
    ) -> Result<Option<SplitCompletionEvidence>, Error> {
        let origin = Instant::now();
        self.completion_revoker.revoke();
        self.completion_revoker = Revoker::new();
        self.current(context)?;
        let SweepReconcile {
            step2: seen,
            after_step2,
            ..
        } = reconcile_recorded(
            &mut self.controller,
            self.services.as_ref(),
            self.policy,
            context,
            &self.generation,
        )
        .await?;
        // #568 S4: step 1 reorged after the step-2 submission withholds
        // completion in every outcome. O1 (re-mined) also fails the recorded
        // block below until it is acknowledged; O2 and O3 (in the mempool,
        // missing) fail the depth below too; an O4 conflict refuses here
        // while it stands: a provisional one is cleared by a reconcile that
        // finds step 1 on Bitcoin (S4-D5), and a terminal one only by one
        // that finds step 1 eligible, six deep (S4-D6), so after that
        // reconcile it stands only while step 1 is not eligible.
        if after_step2 != Step1AfterStep2::Eligible {
            return Ok(None);
        }
        let claim_observation::TransactionObservation::Confirmed { txid, block } = seen else {
            return Ok(None);
        };
        if txid != self.recorded_txid()? {
            return Err(Error::InvalidBinding);
        }
        let recheck = self.recheck_sweep(context, seen).await?;
        let plan = self.controller.plan();
        let observations = recheck.assessment().observations;
        // Positive inclusion evidence on both chains, at the recheck's tips;
        // the journal's assessment is not consulted (an expired RDTS window
        // does not undo a confirmed Split).
        if !completion_bitcoin_confirmed(&plan, observations.bitcoin)
            || observations.fork.chain != plan.fork_chain
            || observations.fork.step1_txid != plan.step1_txid()
            || observations.fork.step1_presence != ForkTransactionPresence::NotObserved
            || !fork_depth_reached(observations.fork.tip, block)
        {
            return Ok(None);
        }
        let not_after = evidence_deadline(
            self.policy,
            observations,
            recheck.observed_at(),
            self.services.source().now(),
            origin,
        )?;
        let identity = self.controller.identity();
        Ok(Some(SplitCompletionEvidence {
            target_cube: identity.fork_cube.clone(),
            source_digest: identity.descriptor_digest,
            fork_chain: plan.fork_chain,
            block,
            txid,
            persisted: AtomicBool::new(false),
            check_revoker: self.completion_revoker.clone(),
            lifetime: Arc::downgrade(&self.lifetime),
            generation: self.generation.clone(),
            expected_generation: context.generation,
            revoker: self.revoker.clone(),
            not_after,
        }))
    }

    /// Reconcile a recorded completion after a fresh loss (D17): step 2 no
    /// longer in the block the record names, or step 1 below its depth,
    /// clears the Cube's matching `split_from` record. Inclusion is inspected
    /// from the recheck's own observations, independently of the journal's
    /// assessment, since an expired RDTS window would otherwise mask a
    /// Bitcoin reorg. No record of this split's step 2 on the Cube is a loss
    /// too, with nothing to clear. Transport failures and a changing view
    /// leave the record alone and return an error; another Split's record is
    /// never cleared; the forgotten descriptors are never restored.
    pub async fn reconcile_split_completion(
        &mut self,
        context: &Context,
        root: &CoincubeDirectory,
        target: &CompletionTarget,
    ) -> Result<SplitCompletionReconciliation, Error> {
        let origin = Instant::now();
        self.completion_revoker.revoke();
        self.completion_revoker = Revoker::new();
        self.current(context)?;
        if target.cube_id != self.controller.identity().fork_cube {
            return Err(Error::InvalidBinding);
        }
        let SweepReconcile {
            status,
            step2: seen,
            ..
        } = reconcile_recorded(
            &mut self.controller,
            self.services.as_ref(),
            self.policy,
            context,
            &self.generation,
        )
        .await?;
        let recheck = self.recheck_sweep(context, seen).await?;
        let plan = self.controller.plan();
        let observations = recheck.assessment().observations;
        let status = completion_bitcoin_loss(&plan, observations.bitcoin)
            .map(Status::Observation)
            .unwrap_or(status);
        let digest = self.controller.identity().descriptor_digest;
        let txid = self.recorded_txid()?;
        let recorded = recorded_height(root, target, digest, txid)?;
        // A completion stands only on this Cube's record of it: with none
        // (an earlier recheck removed it, then expired), it is lost.
        let left_block = match seen {
            claim_observation::TransactionObservation::Confirmed { block, .. } => {
                recorded.is_none_or(|height| height != block.height)
            }
            _ => true,
        };
        let lost = left_block
            || matches!(
                status,
                Status::Observation(
                    Assessment::Reorged
                        | Assessment::WaitingForConfirmation
                        | Assessment::WaitingForDepth { .. }
                )
            );
        if !lost {
            return Ok(SplitCompletionReconciliation::Standing {
                status,
                transaction: seen,
            });
        }
        let deadline = evidence_deadline(
            self.policy,
            observations,
            recheck.observed_at(),
            self.services.source().now(),
            origin,
        )?;
        let cleared = self
            .clear_split_from(context, root, target, digest, txid, deadline)
            .await?;
        Ok(SplitCompletionReconciliation::Lost {
            status,
            transaction: seen,
            cleared,
        })
    }

    /// Remove the target Cube's record of this Split (`digest`, `txid`)
    /// under the BTCB2 writer lock, before `deadline` and while the session
    /// is current. `false` when no such record is there, with nothing
    /// written.
    async fn clear_split_from(
        &mut self,
        context: &Context,
        root: &CoincubeDirectory,
        target: &CompletionTarget,
        digest: sha256::Hash,
        txid: Txid,
        deadline: Instant,
    ) -> Result<bool, Error> {
        let directory = root.network_directory(ChainId::BitcoinBlake2b);
        let apply = |settings: &mut Settings| -> Result<bool, SettingsError> {
            if self.revoker.is_revoked()
                || self.generation.has_changed().is_err()
                || *self.generation.borrow() != context.generation
                || Instant::now() >= deadline
            {
                return Err(SettingsError::Unexpected(
                    "Split reorg check expired or changed".into(),
                ));
            }
            let cube = target.matching_cube(root, settings, ChainId::BitcoinBlake2b, digest)?;
            let before = cube.split_from.len();
            cube.split_from.retain(|record| {
                !(record.descriptor_digest == digest && record.step2_txid == txid)
            });
            Ok(cube.split_from.len() != before)
        };
        let marked = recorded_height(root, target, digest, txid)?.is_some();
        if marked {
            // Refuse a missing or mismatched Cube before the file is touched;
            // the same check runs again under the writer lock.
            let mut snapshot = Settings::from_file(&directory)
                .map_err(|error| Error::CompletionPersistence(error.to_string()))?;
            apply(&mut snapshot)
                .map_err(|error| Error::CompletionPersistence(error.to_string()))?;
            update_settings_file_checked(&directory, |mut settings| {
                apply(&mut settings)?;
                Ok(Some(settings))
            })
            .await
            .map_err(|error| Error::CompletionPersistence(error.to_string()))?;
        }
        self.current(context)?;
        if Instant::now() >= deadline {
            return Err(Error::ExpiredEvidence);
        }
        Ok(marked)
    }
}

/// The height the target Cube's settings record for this Split (`digest`,
/// `txid`), read without a lock; `None` when there is no settings file or no
/// such record. An unreadable file is an error, never "no record".
fn recorded_height(
    root: &CoincubeDirectory,
    target: &CompletionTarget,
    digest: sha256::Hash,
    txid: Txid,
) -> Result<Option<u64>, Error> {
    let directory = root.network_directory(ChainId::BitcoinBlake2b);
    if !directory.path().join(SETTINGS_FILE_NAME).exists() && target.imported.is_none() {
        return Ok(None);
    }
    let mut settings = Settings::from_file(&directory)
        .map_err(|error| Error::CompletionPersistence(error.to_string()))?;
    if target.imported.is_some() {
        target
            .matching_cube(root, &mut settings, ChainId::BitcoinBlake2b, digest)
            .map_err(|error| Error::CompletionPersistence(error.to_string()))?;
    }
    Ok(settings
        .cubes
        .iter()
        .filter(|cube| cube.id == target.cube_id)
        .flat_map(|cube| cube.split_from.iter())
        .find(|record| record.descriptor_digest == digest && record.step2_txid == txid)
        .map(|record| record.completed_height))
}

#[cfg(test)]
mod imported_tests {
    use super::*;
    use crate::app::settings::{BitcoinImport, BitcoinImportStage, WalletSettings};
    use coincube_core::miniscript::bitcoin::hashes::Hash;

    struct PairFixture {
        root: CoincubeDirectory,
        source: CubeSettings,
        target: CubeSettings,
    }
    impl Drop for PairFixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(self.root.path()).unwrap();
        }
    }
    impl PairFixture {
        fn new() -> Self {
            let root = CoincubeDirectory::new(
                std::env::temp_dir().join(format!("imported-completion-{}", uuid::Uuid::new_v4())),
            );
            let (mut source, _, _) = crate::imported_claim::tests::fixture();
            source.master_signer_fingerprint = Some([1, 2, 3, 4].into());
            let import = source.bitcoin_import.as_ref().unwrap();
            let source_wallet = import.active_attachment().unwrap();
            let wallet = ImportedWalletAttachment::new(
                source_wallet.descriptor.paired_blake2b().unwrap(),
                ChainId::BitcoinBlake2b,
                source_wallet.birthday,
                source_wallet.initial_lookahead,
            )
            .unwrap();
            let mut target = CubeSettings::new_with_raw_id(
                crate::services::bitcoin_import::pairing::target_cube_id(&source.id),
                "Synthetic imported pair".into(),
                ChainId::BitcoinBlake2b,
            );
            target.master_signer_fingerprint = source.master_signer_fingerprint;
            target.imported_claim_source = Some(Box::new(ImportedClaimPair {
                source_cube_id: source.id.clone(),
                source_operation_id: import.operation_id,
                source_wallet: source_wallet.clone(),
            }));
            target.bitcoin_import = Some(Box::new(BitcoinImport {
                operation_id: uuid::Uuid::parse_str(&target.id).unwrap(),
                stage: BitcoinImportStage::Ready { wallet },
                last_error: None,
            }));
            let fixture = Self {
                root,
                source,
                target,
            };
            for cube in [&fixture.source, &fixture.target] {
                let wallet = cube
                    .bitcoin_import
                    .as_ref()
                    .unwrap()
                    .active_attachment()
                    .unwrap();
                let metadata = WalletSettings {
                    name: "Synthetic external signer".into(),
                    alias: None,
                    descriptor_checksum: wallet.wallet_id.descriptor_checksum.clone(),
                    pinned_at: wallet.wallet_id.timestamp,
                    keys: Vec::new(),
                    hardware_wallets: Vec::new(),
                    remote_backend_auth: None,
                    start_internal_bitcoind: Some(false),
                    pending_rescan: None,
                    keychain_keys_recorded: false,
                };
                fixture.save(
                    cube.network,
                    &Settings {
                        cubes: vec![cube.clone()],
                        wallets: vec![metadata],
                        ..Default::default()
                    },
                );
            }
            fixture
        }
        fn path(&self, chain: ChainId) -> std::path::PathBuf {
            self.root
                .network_directory(chain)
                .path()
                .join(SETTINGS_FILE_NAME)
        }
        fn save(&self, chain: ChainId, settings: &Settings) {
            let path = self.path(chain);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, serde_json::to_vec_pretty(settings).unwrap()).unwrap();
        }
        fn evidence(&self) -> (SplitCompletionEvidence, Arc<()>, watch::Sender<u64>) {
            let lifetime = Arc::new(());
            let (sender, generation) = watch::channel(7);
            let target = CompletionTarget::of(&self.target).unwrap();
            // Synthetic completion evidence exercises only the settings boundary;
            // the coordinator's real chain checks retain their separate tests.
            let evidence = SplitCompletionEvidence {
                target_cube: target.cube_id.clone(),
                source_digest: target.imported.as_ref().unwrap().source_digest,
                fork_chain: ChainId::BitcoinBlake2b,
                block: BlockRef {
                    height: 100,
                    hash: BlockHash::from_byte_array([1; 32]),
                },
                txid: Txid::from_byte_array([2; 32]),
                persisted: AtomicBool::new(false),
                check_revoker: Revoker::new(),
                lifetime: Arc::downgrade(&lifetime),
                generation,
                expected_generation: 7,
                revoker: Revoker::new(),
                not_after: Instant::now() + Duration::from_secs(30),
            };
            (evidence, lifetime, sender)
        }
    }

    #[tokio::test]
    async fn imported_completion_record_roundtrips_without_replacing_pair() {
        let fixture = PairFixture::new();
        let target = CompletionTarget::of(&fixture.target).unwrap();
        let source_before = std::fs::read(fixture.path(ChainId::Bitcoin)).unwrap();
        let (evidence, _lifetime, _sender) = fixture.evidence();
        evidence.persist(&fixture.root, &target).await.unwrap();
        let written = std::fs::read(fixture.path(ChainId::BitcoinBlake2b)).unwrap();
        evidence.persist(&fixture.root, &target).await.unwrap();
        assert_eq!(
            std::fs::read(fixture.path(ChainId::BitcoinBlake2b)).unwrap(),
            written
        );
        assert_eq!(
            std::fs::read(fixture.path(ChainId::Bitcoin)).unwrap(),
            source_before
        );
        let saved =
            Settings::from_file(&fixture.root.network_directory(ChainId::BitcoinBlake2b)).unwrap();
        let cube = &saved.cubes[0];
        assert_eq!(CompletionTarget::of(cube), Some(target.clone()));
        assert_eq!(cube.bitcoin_import, fixture.target.bitcoin_import);
        assert_eq!(
            cube.imported_claim_source,
            fixture.target.imported_claim_source
        );
        assert!(cube.vault_wallet_id.is_none());
        assert_eq!(cube.split_from, vec![evidence.record()]);
        assert_eq!(
            recorded_height(
                &fixture.root,
                &target,
                evidence.source_digest(),
                evidence.txid()
            )
            .unwrap(),
            Some(100)
        );
        assert!(evidence.persisted.load(Ordering::SeqCst));
        let record = serde_json::to_value(&cube.split_from[0]).unwrap();
        assert_eq!(record.as_object().unwrap().len(), 3);
        assert!(record.get("descriptor_digest").is_some());
        assert!(record.get("completed_height").is_some());
        assert!(record.get("step2_txid").is_some());
    }

    #[tokio::test]
    async fn imported_completion_rejects_changed_pair_source_or_journal() {
        for case in [
            "source_operation",
            "source_wallet",
            "target_operation",
            "target_wallet",
            "target_signer",
            "source_binding",
            "signer_metadata",
            "duplicate_target",
            "duplicate_source",
            "unpaired",
            "native_vault",
            "journal_source",
            "journal_target",
        ] {
            let fixture = PairFixture::new();
            let target = CompletionTarget::of(&fixture.target).unwrap();
            let (mut evidence, _lifetime, _sender) = fixture.evidence();
            let mut source =
                Settings::from_file(&fixture.root.network_directory(ChainId::Bitcoin)).unwrap();
            let mut fork =
                Settings::from_file(&fixture.root.network_directory(ChainId::BitcoinBlake2b))
                    .unwrap();
            match case {
                "source_operation" => {
                    source.cubes[0]
                        .bitcoin_import
                        .as_mut()
                        .unwrap()
                        .operation_id = uuid::Uuid::new_v4()
                }
                "source_wallet" => {
                    let BitcoinImportStage::Ready { wallet } =
                        &mut source.cubes[0].bitcoin_import.as_mut().unwrap().stage
                    else {
                        unreachable!()
                    };
                    wallet.birthday += 1;
                }
                "target_operation" => {
                    fork.cubes[0].bitcoin_import.as_mut().unwrap().operation_id =
                        uuid::Uuid::new_v4()
                }
                "target_wallet" => {
                    let BitcoinImportStage::Ready { wallet } =
                        &mut fork.cubes[0].bitcoin_import.as_mut().unwrap().stage
                    else {
                        unreachable!()
                    };
                    wallet.initial_lookahead += 1;
                }
                "target_signer" => {
                    fork.cubes[0].master_signer_fingerprint = Some([9, 9, 9, 9].into())
                }
                "source_binding" => {
                    fork.cubes[0]
                        .imported_claim_source
                        .as_mut()
                        .unwrap()
                        .source_operation_id = uuid::Uuid::new_v4()
                }
                "signer_metadata" => fork.wallets[0].keychain_keys_recorded = true,
                "duplicate_target" => fork.cubes.push(fork.cubes[0].clone()),
                "duplicate_source" => source.cubes.push(source.cubes[0].clone()),
                "unpaired" => fork.cubes[0].imported_claim_source = None,
                "native_vault" => {
                    fork.cubes[0].vault_wallet_id = Some(target.vault_wallet_id.clone())
                }
                "journal_source" => evidence.source_digest = sha256::Hash::from_byte_array([0; 32]),
                "journal_target" => evidence.target_cube = "another-cube".into(),
                _ => unreachable!(),
            }
            fixture.save(ChainId::Bitcoin, &source);
            fixture.save(ChainId::BitcoinBlake2b, &fork);
            let before = std::fs::read(fixture.path(ChainId::BitcoinBlake2b)).unwrap();
            assert!(
                evidence.persist(&fixture.root, &target).await.is_err(),
                "{}",
                case
            );
            assert!(!evidence.persisted.load(Ordering::SeqCst), "{}", case);
            assert_eq!(
                std::fs::read(fixture.path(ChainId::BitcoinBlake2b)).unwrap(),
                before,
                "{}",
                case
            );
            if case != "journal_target" {
                assert!(
                    recorded_height(
                        &fixture.root,
                        &target,
                        evidence.source_digest(),
                        evidence.txid()
                    )
                    .is_err(),
                    "{}",
                    case
                );
            }
        }
    }

    #[test]
    fn native_completion_target_keeps_existing_vault_matching() {
        let cube = CubeSettings::new_with_raw_id(
            "native-target".into(),
            "Synthetic native Vault".into(),
            ChainId::BitcoinBlake2b,
        )
        .with_vault(crate::app::settings::VaultIdentity {
            wallet_id: WalletId::new("synthetic-checksum".into(), Some(1)),
            fingerprint: Some("0123abcd".into()),
        });
        let target = CompletionTarget::of(&cube).unwrap();
        assert!(target.imported.is_none());
        assert_eq!(Some(&target.vault_wallet_id), cube.vault_wallet_id.as_ref());
        assert_eq!(
            Some(&target.vault_fingerprint),
            cube.vault_fingerprint.as_ref()
        );
        let mut settings = Settings {
            cubes: vec![cube.clone()],
            ..Default::default()
        };
        // Native matching continues to require no imported source or metadata.
        let root =
            CoincubeDirectory::new(std::env::temp_dir().join(uuid::Uuid::new_v4().to_string()));
        let digest = sha256::Hash::from_byte_array([3; 32]);
        assert_eq!(
            target
                .matching_cube(&root, &mut settings, ChainId::BitcoinBlake2b, digest)
                .unwrap()
                .id,
            cube.id
        );
        settings.cubes[0].vault_fingerprint = Some("changed".into());
        assert!(target
            .matching_cube(&root, &mut settings, ChainId::BitcoinBlake2b, digest)
            .is_err());
        settings.cubes[0] = cube;
        settings.cubes[0]
            .vault_wallet_id
            .as_mut()
            .unwrap()
            .descriptor_checksum = "changed".into();
        assert!(target
            .matching_cube(&root, &mut settings, ChainId::BitcoinBlake2b, digest)
            .is_err());
    }

    #[test]
    fn imported_completion_requires_installed_dedicated_pair() {
        let fixture = PairFixture::new();
        assert!(CompletionTarget::of(&fixture.source).is_none());
        let mut pending = fixture.target.clone();
        let wallet = pending
            .bitcoin_import
            .as_ref()
            .unwrap()
            .attachment()
            .unwrap()
            .clone();
        pending.bitcoin_import.as_mut().unwrap().stage = BitcoinImportStage::Installing { wallet };
        assert!(CompletionTarget::of(&pending).is_none());
        let mut arbitrary = fixture.target.clone();
        arbitrary.id = uuid::Uuid::new_v4().to_string();
        assert!(CompletionTarget::of(&arbitrary).is_none());
    }
}
