//! Exact-record admission for a downloaded password-v3 Kit. The device-only
//! anchor commits the entire signer set and checkpoint binding as one manifest
//! transition. An incomplete restore remains non-signing across restart.

use super::*;
use crate::services::{
    coincube::{imported_checkpoint::Checkpoint, CoincubeClient, CubeResponse},
    recovery::{imported_checkpoint::VerifiedDownload, imported_key_v3::ImportedKeyKitV3},
};

/// No signed PSBT escapes until both current server authority and the exact
/// local manifest have been rechecked. This is intentionally non-Clone.
pub(crate) struct RestoredSignature {
    psbt: Psbt,
    binding: RestoredCheckpoint,
}

pub(crate) fn restore_intent_path(directory: &NetworkDirectory, operation_id: Uuid) -> PathBuf {
    directory
        .path()
        .join("imports")
        .join(format!(".imported-v3-{operation_id}.json"))
}

fn remote_matches(
    binding: &RestoredCheckpoint,
    cube_uuid: &str,
    network: &str,
    cube: &CubeResponse,
    status: &Checkpoint,
) -> bool {
    status.validate().is_ok()
        && cube.id == binding.cube_id
        && cube.uuid == cube_uuid
        && cube.network == network
        && cube.status == "active"
        && status.active
        && status.generation == binding.generation
        && status.commitment == binding.commitment
}

impl RestoredSignature {
    /// Call immediately before persistence or broadcast. A Connect GET cannot
    /// reserve the remote epoch: a later revocation may still race the caller.
    pub(crate) async fn release_after_recheck(
        self,
        repository: &LocalKeyRepository<'_>,
        client: &CoincubeClient,
        is_current: impl Fn() -> bool,
    ) -> Result<Psbt, String> {
        if !is_current() || repository.current_restored_binding()? != self.binding {
            return Err("Restored signing authority changed before release.".into());
        }
        repository
            .check_remote_binding(client, &self.binding)
            .await?;
        if !is_current() || repository.current_restored_binding()? != self.binding {
            return Err("Restored signing authority changed before release.".into());
        }
        Ok(self.psbt)
    }
}

impl LocalKeyRepository<'_> {
    fn current_restored_binding(&self) -> Result<RestoredCheckpoint, String> {
        self.check_active()?;
        match fs::symlink_metadata(restore_intent_path(self.directory, self.operation_id)) {
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            _ => return Err("Finish the imported key restore before signing.".into()),
        }
        let lock = self.lock()?;
        let manifest = self.read_manifest_recoverable(&lock)?;
        let device = self.device(false)?;
        let credential = self.check_active()?;
        self.verified_restored_binding(&manifest, &credential, &device, || {})
    }

    pub(super) fn verified_restored_binding(
        &self,
        manifest: &Manifest,
        credential: &str,
        device: &coincube_core::seed_crypt::DeviceSecret,
        after_verification: impl FnOnce(),
    ) -> Result<RestoredCheckpoint, String> {
        let binding = manifest
            .restored
            .as_ref()
            .filter(|binding| binding.ready)
            .cloned()
            .ok_or("Restored signer inventory is not ready.")?;
        self.verify_records(manifest, credential, device)?;
        after_verification();
        self.check_active()?;
        Ok(binding)
    }

    /// Release a restored signer set only after observing a strictly later
    /// inactive server epoch for its exact owner Cube. All restored records
    /// retire with the binding in one anchored transition, so the legacy
    /// offline signer cannot inherit them after release. This performs no
    /// remote mutation; the caller must revoke the checkpoint first.
    pub(crate) async fn release_restored_after_revocation(
        &self,
        client: &CoincubeClient,
        is_current: impl Fn() -> bool,
    ) -> Result<(), String> {
        if !is_current() {
            return Err("Imported restore release was cancelled.".into());
        }
        let expected = self.current_restored_binding()?;
        let cube = client
            .get_imported_checkpoint_cube(expected.cube_id)
            .await
            .map_err(|_| "Cannot verify restored Cube owner for release.")?;
        let remote = client
            .get_imported_checkpoint(expected.cube_id)
            .await
            .map_err(|_| "Cannot verify restored checkpoint revocation.")?;
        if !is_current() {
            return Err("Imported restore release was cancelled.".into());
        }
        let credential = self.check_active()?;
        let lock = self.lock()?;
        let manifest = self.read_manifest_recoverable(&lock)?;
        let device = self.device(false)?;
        self.release_restored_locked(
            manifest,
            &expected,
            &cube,
            &remote,
            &credential,
            &device,
            || is_current() && self.check_active().is_ok(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn release_restored_locked(
        &self,
        manifest: Manifest,
        expected: &RestoredCheckpoint,
        cube: &CubeResponse,
        remote: &Checkpoint,
        credential: &str,
        device: &coincube_core::seed_crypt::DeviceSecret,
        is_current: impl Fn() -> bool,
    ) -> Result<(), String> {
        if !is_current() {
            return Err("Imported restore release was cancelled.".into());
        }
        remote
            .validate()
            .map_err(|_| "Invalid restored checkpoint revocation proof.")?;
        if manifest.restored.as_ref() != Some(expected)
            || !expected.ready
            || manifest.checkpoint.is_some()
            || cube.id != expected.cube_id
            || cube.uuid != self.cube.id
            || cube.network != self.cube.network.api_str()
            || cube.status != "active"
            || remote.active
            || remote.generation <= expected.generation
        {
            return Err("Restored checkpoint has no later inactive owner-scoped epoch.".into());
        }
        self.verify_records(&manifest, credential, device)?;
        self.check_active()?;
        if !is_current() {
            return Err("Imported restore release was cancelled.".into());
        }
        let next = self.manifest(vec![]);
        self.publish_manifest(&manifest, &next, credential, device, &is_current)?;
        self.check_active()?;
        if !is_current() {
            return Err("Imported restore release was cancelled.".into());
        }
        Ok(())
    }

    async fn check_remote_binding(
        &self,
        client: &CoincubeClient,
        binding: &RestoredCheckpoint,
    ) -> Result<(), String> {
        let cube = client
            .get_imported_checkpoint_cube(binding.cube_id)
            .await
            .map_err(|_| "Cannot verify restored Cube authority.")?;
        let status = client
            .get_imported_checkpoint(binding.cube_id)
            .await
            .map_err(|_| "Cannot verify current imported checkpoint.")?;
        if !remote_matches(
            binding,
            &self.cube.id,
            self.cube.network.api_str(),
            &cube,
            &status,
        ) {
            return Err("Imported checkpoint was revoked or replaced.".into());
        }
        Ok(())
    }

    /// Local signers restored from v3 are denied by the synchronous `sign`
    /// entrypoint. This wrapper requires live Connect authority before and
    /// after the expensive signing operation, then holds the PSBT privately.
    pub(crate) async fn sign_restored(
        &self,
        client: &CoincubeClient,
        record_id: Uuid,
        expected_revision: Uuid,
        original: &Psbt,
        current: &Psbt,
        is_current: impl Fn() -> bool,
    ) -> Result<RestoredSignature, String> {
        if !is_current() {
            return Err("Imported signing was cancelled.".into());
        }
        let binding = self.current_restored_binding()?;
        self.check_remote_binding(client, &binding).await?;
        if !is_current() || self.current_restored_binding()? != binding {
            return Err("Restored signing authority changed.".into());
        }
        let psbt = self.with_key(
            record_id,
            expected_revision,
            Some(&binding),
            &is_current,
            |key| {
                key.sign(&self.attachment.descriptor, original, current)
                    .map_err(|_| "Imported signing failed verification.".into())
            },
        )?;
        self.check_remote_binding(client, &binding).await?;
        if !is_current() || self.current_restored_binding()? != binding {
            return Err("Restored signing authority changed after signing.".into());
        }
        Ok(RestoredSignature { psbt, binding })
    }

    /// A staged batch becomes locally ready only after a live Connect check
    /// immediately before and after the anchor transition. The durable intent
    /// still blocks signer use until the coordinator removes it after success.
    pub(crate) async fn finish_restored_batch_verified(
        &self,
        client: &CoincubeClient,
        cube_id: u64,
        checkpoint: &Checkpoint,
        source_inventory_generation: u64,
        is_current: impl Fn() -> bool,
    ) -> Result<(), String> {
        let binding = RestoredCheckpoint {
            cube_id,
            generation: checkpoint.generation,
            commitment: checkpoint.commitment.clone(),
            source_inventory_generation,
            ready: true,
        };
        if !is_current() {
            return Err("Imported restore was cancelled.".into());
        }
        self.check_remote_binding(client, &binding).await?;
        self.finish_restored_batch(
            cube_id,
            checkpoint,
            source_inventory_generation,
            &is_current,
        )?;
        self.check_remote_binding(client, &binding).await?;
        if !is_current() {
            return Err("Imported restore was cancelled.".into());
        }
        Ok(())
    }

    pub(crate) fn admit_restored_batch(
        &self,
        download: VerifiedDownload,
        is_current: impl Fn() -> bool,
    ) -> Result<Vec<LocalKeyRecord>, String> {
        let (cube_id, remote, kit) = download.into_parts();
        if !is_current() || self.check_active().is_err() {
            return Err("Imported restore is no longer current.".into());
        }
        kit.validate()?;
        remote
            .validate()
            .map_err(|_| "Invalid remote checkpoint.")?;
        if !remote.active
            || cube_id == 0
            || remote.generation != kit.checkpoint_generation
            || kit.public.cube.uuid != self.cube.id
            || kit.public.wallet.descriptor != self.attachment.descriptor
            || kit.public.import_operation_id != Some(self.operation_id)
            || kit.wallet_id != self.attachment.wallet_id.to_string()
            || kit.policy_hash != hex::encode(self.policy_hash)
        {
            return Err("Imported restore scope or checkpoint changed.".into());
        }
        let binding = RestoredCheckpoint {
            cube_id,
            generation: remote.generation,
            commitment: remote.commitment.clone(),
            source_inventory_generation: kit.inventory_generation,
            ready: false,
        };
        self.admit_restored_batch_with_cube(kit, binding, is_current)
    }

    fn admit_restored_batch_with_cube(
        &self,
        kit: ImportedKeyKitV3,
        binding: RestoredCheckpoint,
        is_current: impl Fn() -> bool,
    ) -> Result<Vec<LocalKeyRecord>, String> {
        let credential = self.check_active()?;
        let lock = self.lock()?;
        self.provision_empty()?;
        let manifest = self.read_manifest_recoverable(&lock)?;
        let device = self.device(manifest.records.is_empty())?;
        self.admit_restored_batch_locked(manifest, kit, binding, &credential, &device, || {
            is_current() && self.check_active().is_ok()
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn admit_restored_batch_locked(
        &self,
        manifest: Manifest,
        kit: ImportedKeyKitV3,
        binding: RestoredCheckpoint,
        credential: &str,
        device: &coincube_core::seed_crypt::DeviceSecret,
        is_current: impl Fn() -> bool,
    ) -> Result<Vec<LocalKeyRecord>, String> {
        if manifest.checkpoint.is_some() || !is_current() {
            return Err("Imported restore inventory is unavailable.".into());
        }
        let mut records = kit
            .local_keys
            .iter()
            .map(|key| key.record.clone())
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            (&left.fingerprint, &left.origin, &left.xpub).cmp(&(
                &right.fingerprint,
                &right.origin,
                &right.xpub,
            ))
        });
        if !manifest.records.is_empty() {
            let mut expected = binding;
            expected.ready = manifest
                .restored
                .as_ref()
                .is_some_and(|stored| stored.ready);
            if manifest.records != records || manifest.restored.as_ref() != Some(&expected) {
                return Err("Existing imported signer state conflicts with this Kit.".into());
            }
            self.verify_records(&manifest, credential, device)?;
            return Ok(records);
        }
        if manifest.restored.is_some() {
            return Err("Empty restored inventory has an unexpected binding.".into());
        }
        let mut ciphertexts = Vec::with_capacity(kit.local_keys.len());
        for key in &kit.local_keys {
            let account = key.record.account()?;
            self.verify_batch_account(&account, &key.secret)?;
            let ciphertext = imported_key_crypt::seal(
                &key.secret,
                credential,
                &self.binding(&key.record)?,
                Protection::DeviceBound(device),
            )
            .map_err(|_| "Cannot reseal restored imported key.")?;
            ciphertexts.push((key.record.clone(), ciphertext));
        }
        let mut next = self.manifest(records.clone());
        next.restored = Some(binding);
        self.publish_manifest_with_stage(
            &manifest,
            &next,
            credential,
            device,
            &is_current,
            || {
                #[cfg(not(test))]
                self.check_active().map(|_| ())?;
                Ok(())
            },
            true,
            || {
                for (record, ciphertext) in ciphertexts {
                    write_new(&self.root().join(record.file_name()), &ciphertext)?;
                }
                Ok(())
            },
        )?;
        Ok(records)
    }

    pub(crate) fn finish_restored_batch(
        &self,
        cube_id: u64,
        checkpoint: &Checkpoint,
        source_inventory_generation: u64,
        is_current: impl Fn() -> bool,
    ) -> Result<(), String> {
        let credential = self.check_active()?;
        let lock = self.lock()?;
        let manifest = self.read_manifest_recoverable(&lock)?;
        let device = self.device(false)?;
        self.finish_restored_batch_locked(
            manifest,
            cube_id,
            checkpoint,
            source_inventory_generation,
            &credential,
            &device,
            is_current,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn finish_restored_batch_locked(
        &self,
        manifest: Manifest,
        cube_id: u64,
        checkpoint: &Checkpoint,
        source_inventory_generation: u64,
        credential: &str,
        device: &coincube_core::seed_crypt::DeviceSecret,
        is_current: impl Fn() -> bool,
    ) -> Result<(), String> {
        self.verify_records(&manifest, credential, device)?;
        let mut bound = RestoredCheckpoint {
            cube_id,
            generation: checkpoint.generation,
            commitment: checkpoint.commitment.clone(),
            source_inventory_generation,
            ready: false,
        };
        bound.ready = manifest
            .restored
            .as_ref()
            .is_some_and(|stored| stored.ready);
        if manifest.restored.as_ref() != Some(&bound) || !is_current() {
            return Err("Restored signer state changed before readiness.".into());
        }
        if bound.ready {
            return Ok(());
        }
        let mut next = manifest.clone();
        next.restored
            .as_mut()
            .ok_or("Missing restored signer binding.")?
            .ready = true;
        self.publish_manifest_with_check(&manifest, &next, credential, device, &is_current, || {
            #[cfg(not(test))]
            self.check_active().map(|_| ())?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_signer_requires_exact_active_server_tuple() {
        let binding = RestoredCheckpoint {
            cube_id: 42,
            generation: 3,
            commitment: "a".repeat(64),
            source_inventory_generation: 7,
            ready: true,
        };
        let mut cube: CubeResponse = serde_json::from_value(serde_json::json!({
            "id": 42, "uuid": "synthetic-cube", "network": "regtest", "name": "Synthetic", "status": "active"
        })).unwrap();
        let mut status = Checkpoint {
            generation: 3,
            commitment: binding.commitment.clone(),
            active: true,
        };
        assert!(remote_matches(
            &binding,
            "synthetic-cube",
            "regtest",
            &cube,
            &status
        ));
        status.generation = 4;
        assert!(!remote_matches(
            &binding,
            "synthetic-cube",
            "regtest",
            &cube,
            &status
        ));
        status.generation = 3;
        status.active = false;
        assert!(!remote_matches(
            &binding,
            "synthetic-cube",
            "regtest",
            &cube,
            &status
        ));
        status.active = true;
        status.commitment = "b".repeat(64);
        assert!(!remote_matches(
            &binding,
            "synthetic-cube",
            "regtest",
            &cube,
            &status
        ));
        status.commitment = binding.commitment.clone();
        cube.uuid = "other-cube".into();
        assert!(!remote_matches(
            &binding,
            "synthetic-cube",
            "regtest",
            &cube,
            &status
        ));
        cube.uuid = "synthetic-cube".into();
        cube.status = "deleted".into();
        assert!(!remote_matches(
            &binding,
            "synthetic-cube",
            "regtest",
            &cube,
            &status
        ));
    }
}
