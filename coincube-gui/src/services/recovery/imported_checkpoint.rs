//! Staged password-v3 checkpoint integration. A verified download is a
//! point-in-time freshness proof, not signer authority or a completed restore.
//! Local atomic admission and persisted signer-use rechecks remain separate.
#![allow(dead_code)]

use std::fmt;

use coincube_core::miniscript::bitcoin::hashes::{sha256, Hash};
use zeroize::Zeroizing;

use crate::services::coincube::{
    imported_checkpoint::{Checkpoint, CheckpointError, MAX_CIPHERTEXT_BYTES},
    CoincubeClient,
};

use super::imported_key_v3::ImportedKeyKitV3;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ImportedCheckpointError {
    Transport(CheckpointError),
    /// A mutation may have committed. Retain the immutable publication and
    /// reconcile by status; never re-encrypt or increment based on this error.
    OutcomeUnknown(CheckpointError),
    Stale,
    InvalidKit,
}

impl From<CheckpointError> for ImportedCheckpointError {
    fn from(error: CheckpointError) -> Self {
        Self::Transport(error)
    }
}

/// One immutable ciphertext for one compare-and-swap attempt. Intentionally
/// neither Clone nor serializable into the UI's restore messages.
pub(crate) struct PreparedPublication {
    cube_id: u64,
    expected_generation: u64,
    ciphertext: String,
    checkpoint: Checkpoint,
}

impl fmt::Debug for PreparedPublication {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreparedPublication")
            .finish_non_exhaustive()
    }
}

fn commitment(ciphertext: &str) -> String {
    hex::encode(sha256::Hash::hash(ciphertext.as_bytes()).to_byte_array())
}

fn require_current(
    expected: &Checkpoint,
    current: &Checkpoint,
) -> Result<(), ImportedCheckpointError> {
    expected.validate()?;
    current.validate()?;
    if expected.generation == 0 || !expected.active || expected != current {
        return Err(ImportedCheckpointError::Stale);
    }
    Ok(())
}

impl PreparedPublication {
    /// The source inventory generation is preserved. The *distinct* Connect
    /// epoch is authenticated inside the ciphertext before publication.
    pub fn encrypt(
        cube_id: u64,
        cube_uuid: &str,
        expected_generation: u64,
        mut kit: ImportedKeyKitV3,
        password: &Zeroizing<String>,
    ) -> Result<Self, ImportedCheckpointError> {
        if cube_id == 0 || kit.public.cube.uuid != cube_uuid {
            return Err(ImportedCheckpointError::InvalidKit);
        }
        let generation = expected_generation
            .checked_add(1)
            .ok_or(ImportedCheckpointError::InvalidKit)?;
        kit.checkpoint_generation = generation;
        let ciphertext = kit
            .encrypt_password(password)
            .map_err(|_| ImportedCheckpointError::InvalidKit)?;
        let checkpoint = Checkpoint {
            generation,
            commitment: commitment(&ciphertext),
            active: true,
        };
        Ok(Self {
            cube_id,
            expected_generation,
            ciphertext,
            checkpoint,
        })
    }

    /// Sends PUT exactly once. On an uncertain response the caller keeps this
    /// object and uses `reconcile`; there is no legacy POST fallback.
    pub async fn publish(&self, client: &CoincubeClient) -> Result<(), ImportedCheckpointError> {
        match client
            .put_imported_checkpoint(self.cube_id, self.expected_generation, &self.ciphertext)
            .await
        {
            Ok(current) => require_current(&self.checkpoint, &current),
            Err(error @ (CheckpointError::Unavailable | CheckpointError::InvalidResponse)) => {
                Err(ImportedCheckpointError::OutcomeUnknown(error))
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Success only if the authenticated current status proves this exact
    /// ciphertext and epoch won. A competing ciphertext at the same epoch, a
    /// tombstone, a later epoch, or an unavailable read never counts as success.
    pub async fn reconcile(&self, client: &CoincubeClient) -> Result<(), ImportedCheckpointError> {
        let current = client.get_imported_checkpoint(self.cube_id).await?;
        require_current(&self.checkpoint, &current)
    }
}

/// Holds secret-bearing v3 material outside cloneable UI types. There is
/// deliberately no `into_keys` or admission method until the restore transaction
/// can persist the checkpoint binding and enforce it at every signer use.
pub(crate) struct VerifiedDownload {
    cube_id: u64,
    checkpoint: Checkpoint,
    kit: ImportedKeyKitV3,
}

impl fmt::Debug for VerifiedDownload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifiedDownload").finish_non_exhaustive()
    }
}

impl VerifiedDownload {
    /// Must be called again at the future admission boundary. This only
    /// observes server state; it cannot make later local use atomic with it.
    pub async fn recheck(&self, client: &CoincubeClient) -> Result<(), ImportedCheckpointError> {
        let current = client.get_imported_checkpoint(self.cube_id).await?;
        require_current(&self.checkpoint, &current)
    }
}

pub(crate) async fn fetch_verified_password_kit(
    client: &CoincubeClient,
    cube_id: u64,
    cube_uuid: &str,
    password: &Zeroizing<String>,
) -> Result<VerifiedDownload, ImportedCheckpointError> {
    let downloaded = client.get_imported_checkpoint_ciphertext(cube_id).await?;
    let checkpoint = Checkpoint {
        generation: downloaded.generation,
        commitment: downloaded.commitment,
        active: downloaded.active,
    };
    require_current(&checkpoint, &checkpoint)?;
    if downloaded.ciphertext.is_empty()
        || downloaded.ciphertext.len() > MAX_CIPHERTEXT_BYTES
        || commitment(&downloaded.ciphertext) != checkpoint.commitment
    {
        return Err(ImportedCheckpointError::InvalidKit);
    }
    let kit = ImportedKeyKitV3::decrypt_password(&downloaded.ciphertext, password, cube_uuid)
        .map_err(|_| ImportedCheckpointError::InvalidKit)?;
    if kit.checkpoint_generation != checkpoint.generation {
        return Err(ImportedCheckpointError::Stale);
    }
    let verified = VerifiedDownload {
        cube_id,
        checkpoint,
        kit,
    };
    // Decryption is expensive. Recheck after it so revocation/replacement
    // during the KDF cannot turn the downloaded snapshot into current proof.
    verified.recheck(client).await?;
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::{Method as MockMethod, MockServer};
    use serde_json::json;

    fn client(server: &MockServer) -> CoincubeClient {
        let mut client = CoincubeClient::for_test(server.base_url());
        client.set_token("synthetic-test-token");
        client
    }

    fn prepared() -> PreparedPublication {
        PreparedPublication::encrypt(
            42,
            "42",
            0,
            super::super::imported_key_v3::tests::sample(),
            &Zeroizing::new("test recovery password".into()),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn checkpoint_uncertain_publish_reconciles_exact_ciphertext_without_second_write() {
        let prepared = prepared();
        let server = MockServer::start();
        let publish = server.mock(|when, then| {
            when.method(MockMethod::PUT)
                .json_body(json!({"expectedGeneration": 0, "ciphertext": prepared.ciphertext}));
            // A commit can succeed before a gateway loses its response.
            then.status(502);
        });
        let status = server.mock(|when, then| {
            when.method(MockMethod::GET).path("/api/v1/connect/cubes/42/recovery-kit/imported-checkpoint");
            then.status(200).json_body(json!({"success": true, "data": {"generation": 1, "commitment": prepared.checkpoint.commitment, "active": true}}));
        });
        let client = client(&server);
        assert_eq!(
            prepared.publish(&client).await,
            Err(ImportedCheckpointError::OutcomeUnknown(
                CheckpointError::Unavailable
            ))
        );
        prepared.reconcile(&client).await.unwrap();
        publish.assert_hits(1);
        status.assert_hits(1);
        assert!(!format!("{prepared:?}").contains(&prepared.ciphertext));
    }

    #[tokio::test]
    async fn checkpoint_reconciliation_rejects_conflicting_stale_tombstoned_and_absent_epochs() {
        let prepared = prepared();
        for (generation, digest, active) in [
            (1, "a".repeat(64), true),
            (2, prepared.checkpoint.commitment.clone(), true),
            (1, prepared.checkpoint.commitment.clone(), false),
            (0, String::new(), false),
        ] {
            let server = MockServer::start();
            server.mock(|when, then| {
                when.method(MockMethod::GET);
                then.status(200).json_body(json!({"success": true, "data": {"generation": generation, "commitment": digest, "active": active}}));
            });
            assert_eq!(
                prepared.reconcile(&client(&server)).await,
                Err(ImportedCheckpointError::Stale)
            );
        }
    }

    #[tokio::test]
    async fn checkpoint_verified_download_keeps_source_inventory_epoch_separate() {
        let prepared = prepared();
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(MockMethod::GET).path("/api/v1/connect/cubes/42/recovery-kit/imported-checkpoint/ciphertext");
            then.status(200).json_body(json!({"success": true, "data": {"generation": 1, "commitment": prepared.checkpoint.commitment, "active": true, "ciphertext": prepared.ciphertext}}));
        });
        let mut status = server.mock(|when, then| {
            when.method(MockMethod::GET).path("/api/v1/connect/cubes/42/recovery-kit/imported-checkpoint");
            then.status(200).json_body(json!({"success": true, "data": {"generation": 1, "commitment": prepared.checkpoint.commitment, "active": true}}));
        });
        let client = client(&server);
        let verified = fetch_verified_password_kit(
            &client,
            42,
            "42",
            &Zeroizing::new("test recovery password".into()),
        )
        .await
        .unwrap();
        assert_eq!(verified.kit.inventory_generation, 3);
        assert_eq!(verified.kit.checkpoint_generation, 1);
        assert!(!format!("{verified:?}").contains("words"));
        status.delete();
        server.mock(|when, then| {
            when.method(MockMethod::GET).path("/api/v1/connect/cubes/42/recovery-kit/imported-checkpoint");
            then.status(200).json_body(json!({"success": true, "data": {"generation": 2, "commitment": prepared.checkpoint.commitment, "active": false}}));
        });
        assert_eq!(
            verified.recheck(&client).await,
            Err(ImportedCheckpointError::Stale)
        );
    }

    #[tokio::test]
    async fn checkpoint_download_rejects_old_ciphertext_republished_at_new_epoch() {
        let prepared = prepared();
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(MockMethod::GET);
            then.status(200).json_body(json!({"success": true, "data": {"generation": 2, "commitment": prepared.checkpoint.commitment, "active": true, "ciphertext": prepared.ciphertext}}));
        });
        assert!(matches!(
            fetch_verified_password_kit(
                &client(&server),
                42,
                "42",
                &Zeroizing::new("test recovery password".into())
            )
            .await,
            Err(ImportedCheckpointError::Stale)
        ));
    }

    #[tokio::test]
    async fn checkpoint_download_rechecks_status_after_decryption() {
        let prepared = prepared();
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(MockMethod::GET).path("/api/v1/connect/cubes/42/recovery-kit/imported-checkpoint/ciphertext");
            then.status(200).json_body(json!({"success": true, "data": {"generation": 1, "commitment": prepared.checkpoint.commitment, "active": true, "ciphertext": prepared.ciphertext}}));
        });
        server.mock(|when, then| {
            when.method(MockMethod::GET).path("/api/v1/connect/cubes/42/recovery-kit/imported-checkpoint");
            then.status(200).json_body(json!({"success": true, "data": {"generation": 2, "commitment": prepared.checkpoint.commitment, "active": false}}));
        });
        assert!(matches!(
            fetch_verified_password_kit(
                &client(&server),
                42,
                "42",
                &Zeroizing::new("test recovery password".into())
            )
            .await,
            Err(ImportedCheckpointError::Stale)
        ));
    }

    #[test]
    fn checkpoint_publication_rejects_mismatched_cube_and_epoch_overflow() {
        let password = Zeroizing::new("unused".into());
        for (cube_id, cube_uuid, generation) in
            [(42, "other", 0), (0, "42", 0), (42, "42", u64::MAX)]
        {
            assert!(matches!(
                PreparedPublication::encrypt(
                    cube_id,
                    cube_uuid,
                    generation,
                    super::super::imported_key_v3::tests::sample(),
                    &password
                ),
                Err(ImportedCheckpointError::InvalidKit)
            ));
        }
    }

    #[tokio::test]
    async fn checkpoint_download_rejects_exact_bytes_mismatch_before_decryption() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(MockMethod::GET);
            then.status(200).json_body(json!({"success": true, "data": {"generation": 1, "commitment": commitment("ciphertext"), "active": true, "ciphertext": "ciphertext\n"}}));
        });
        assert!(matches!(
            fetch_verified_password_kit(
                &client(&server),
                42,
                "42",
                &Zeroizing::new("unused".into())
            )
            .await,
            Err(ImportedCheckpointError::InvalidKit)
        ));
    }
}
