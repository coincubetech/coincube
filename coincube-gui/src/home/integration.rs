//! Opt-in driver for the real PIN Cube creation and backup ceremony.
//! The caller owns a durable private backup before invoking this driver.

use super::*;
use iced_runtime::futures::futures::StreamExt;

async fn drain(task: Task<Message>) -> Vec<Message> {
    match iced_runtime::task::into_stream(task) {
        Some(stream) => {
            stream
                .filter_map(|action| async move {
                    match action {
                        iced_runtime::Action::Output(message) => Some(message),
                        _ => None,
                    }
                })
                .collect()
                .await
        }
        None => Vec::new(),
    }
}

/// Persist a run-owned Cube through Home's real validation/backup/finalization.
/// Re-entry verifies the existing sealed seed instead of replacing it.
pub async fn create_testnet4_cube(
    root: CoincubeDirectory,
    id: uuid::Uuid,
    name: String,
    pin: &str,
    saved_words: &[String],
    server_limit: usize,
) -> Result<CubeSettings, &'static str> {
    let network_dir = root.network_directory(Network::Testnet4);
    let existing = match settings::Settings::from_file(&network_dir) {
        Ok(settings) => settings.cubes,
        Err(settings::SettingsError::NotFound) => Vec::new(),
        Err(_) => return Err("cube_settings_unreadable"),
    };
    let cube = if let Some(cube) = existing.iter().find(|c| c.id == id.to_string()) {
        if cube.name != name || cube.network != ChainId::Testnet4 {
            return Err("cube_identity_mismatch");
        }
        cube.clone()
    } else {
        let (mut home, _) = Home::new(root.clone(), Some(Network::Testnet4));
        home.network = ChainId::Testnet4;
        home.state = State::Cubes {
            cubes: existing,
            create_cube: true,
            source: ChainId::Testnet4,
        };
        home.server_cube_limit = Some(server_limit);
        home.passkey_mode = false;
        home.pending_cube_id = Some(id);
        home.create_cube_name.value = name;
        for (index, digit) in pin.chars().enumerate() {
            let message = pin_input::Message::DigitChanged(index, digit.to_string());
            let _ = home.create_cube_pin.update(message.clone());
            let _ = home.create_cube_pin_confirm.update(message);
        }
        let _ = drain(home.update(Message::View(ViewMessage::CreateCube))).await;
        if !matches!(
            home.state,
            State::CreationBackup(CreationBackupStep::Choice)
        ) {
            return Err("cube_creation_refused");
        }
        // Replace only the ephemeral pre-persistence words with the run's
        // already saved random phrase. This makes an interrupted finalize
        // retry use the same identity; no settings or sealed seed is replaced.
        let mnemonic = bip39::Mnemonic::parse_in(bip39::Language::English, saved_words.join(" "))
            .map_err(|_| "invalid_saved_backup")?;
        if mnemonic.word_count() != 12 {
            return Err("invalid_saved_backup");
        }
        home.creation_backup_words = Some(zeroize::Zeroizing::new(saved_words.to_vec()));
        let backup = |message| Message::View(ViewMessage::CreationBackup(message));
        let _ = home.update(backup(BackupWalletMessage::NextStep));
        let _ = home.update(backup(BackupWalletMessage::ToggleBackupIntroCheck));
        let _ = home.update(backup(BackupWalletMessage::NextStep));
        let _ = home.update(backup(BackupWalletMessage::NextStep));
        let State::CreationBackup(CreationBackupStep::Verification { word_indices, .. }) =
            home.state.clone()
        else {
            return Err("backup_verification_unavailable");
        };
        for index in word_indices {
            let _ = home.update(backup(BackupWalletMessage::WordInput {
                index: index as u8,
                input: saved_words[index - 1].clone(),
            }));
        }
        let messages = drain(home.update(backup(BackupWalletMessage::VerifyPhrase))).await;
        let mut created = None;
        for message in messages {
            if let Message::CubeCreated(result) = message {
                created = Some(result.map_err(|_| "cube_persistence_failed")?);
            }
        }
        created.ok_or("cube_persistence_failed")?
    };
    if !cube.backed_up
        || cube.creation_backup_bypass.is_some()
        || creation_gate::evaluate_for_cube(&cube, None) != creation_gate::CreationGate::Satisfied
    {
        return Err("cube_backup_not_verified");
    }
    let location = unlock::CubeLocation::new(root.path(), &cube);
    let signer = match unlock::unlock_blocking(&location, pin).map_err(|_| "cube_unlock_failed")? {
        unlock::PinOutcome::Unlock(signer) => signer,
        _ => return Err("cube_unlock_failed"),
    };
    if signer.words() != saved_words {
        return Err("cube_backup_mismatch");
    }
    Ok(cube)
}

#[cfg(test)]
mod tests {
    use super::create_testnet4_cube;
    use crate::app::settings::{self, CubeSettings};
    use crate::chain::ChainId;
    use crate::dir::CoincubeDirectory;
    use crate::services::unlock::creation_gate::{self, CreationBackupBypass, CreationRecoveryKit};
    use coincube_core::miniscript::bitcoin::Network;

    fn temp_root(tag: &str) -> CoincubeDirectory {
        let path = std::env::temp_dir().join(format!(
            "coincube-testnet4-reentry-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        CoincubeDirectory::new(path)
    }

    fn write_cubes(root: &CoincubeDirectory, cubes: Vec<CubeSettings>) -> Vec<u8> {
        let directory = root.network_directory(Network::Testnet4);
        std::fs::create_dir_all(directory.path()).unwrap();
        let settings = settings::Settings {
            cubes,
            ..settings::Settings::default()
        };
        let bytes = serde_json::to_vec_pretty(&settings).unwrap();
        std::fs::write(directory.path().join(settings::SETTINGS_FILE_NAME), &bytes).unwrap();
        bytes
    }

    fn reenter(
        root: CoincubeDirectory,
        id: uuid::Uuid,
        name: &str,
    ) -> Result<CubeSettings, &'static str> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(create_testnet4_cube(
            root,
            id,
            name.to_string(),
            "0000",
            &[],
            1,
        ))
    }

    fn assert_settings_unchanged(root: &CoincubeDirectory, before: &[u8]) {
        let path = root
            .network_directory(Network::Testnet4)
            .path()
            .join(settings::SETTINGS_FILE_NAME);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn reentry_refuses_a_different_cube_identity_without_rewriting_settings() {
        let id = uuid::Uuid::new_v4();
        for (label, cube) in [
            (
                "name",
                CubeSettings::new_with_id(id, "Other".to_string(), ChainId::Testnet4),
            ),
            (
                "chain",
                CubeSettings::new_with_id(id, "E2E".to_string(), ChainId::Bitcoin),
            ),
        ] {
            let root = temp_root(label);
            let before = write_cubes(&root, vec![cube]);
            let error = reenter(root.clone(), id, "E2E").unwrap_err();
            assert_eq!(error, "cube_identity_mismatch", "{label}");
            assert_settings_unchanged(&root, &before);
            std::fs::remove_dir_all(root.path()).unwrap();
        }
    }

    #[test]
    fn reentry_requires_a_verified_phrase_and_refuses_a_bypass_or_kit_substitute() {
        let id = uuid::Uuid::new_v4();
        let mut unverified = CubeSettings::new_with_id(id, "E2E".to_string(), ChainId::Testnet4);
        unverified.creation_backup_required = true;

        let mut bypassed = unverified.clone();
        bypassed.backed_up = true;
        bypassed.creation_backup_bypass = Some(CreationBackupBypass {
            at: 1_700_000_000,
            acknowledged: creation_gate::BYPASS_ACKNOWLEDGEMENT.to_string(),
        });

        // A seed-bearing creation kit satisfies the open gate, and a legacy
        // cube is not gated at all. Neither is the phrase this driver re-reads.
        let mut kit_only = unverified.clone();
        kit_only.creation_recovery_kit = Some(CreationRecoveryKit {
            at: 1_700_000_000,
            cube_id: 7,
            has_seed: true,
        });
        let mut legacy = CubeSettings::new_with_id(id, "E2E".to_string(), ChainId::Testnet4);
        legacy.backed_up = false;

        for (label, cube) in [
            ("unverified", unverified),
            ("bypassed", bypassed),
            ("kit", kit_only),
            ("legacy", legacy),
        ] {
            let root = temp_root(label);
            let before = write_cubes(&root, vec![cube]);
            let error = reenter(root.clone(), id, "E2E").unwrap_err();
            assert_eq!(error, "cube_backup_not_verified", "{label}");
            assert_settings_unchanged(&root, &before);
            std::fs::remove_dir_all(root.path()).unwrap();
        }
    }

    #[test]
    fn reentry_refuses_unreadable_settings_without_creating_a_cube() {
        let root = temp_root("corrupt");
        let directory = root.network_directory(Network::Testnet4);
        std::fs::create_dir_all(directory.path()).unwrap();
        let path = directory.path().join(settings::SETTINGS_FILE_NAME);
        std::fs::write(&path, b"{").unwrap();

        let error = reenter(root.clone(), uuid::Uuid::new_v4(), "E2E").unwrap_err();
        assert_eq!(error, "cube_settings_unreadable");
        assert_eq!(std::fs::read(&path).unwrap(), b"{");
        std::fs::remove_dir_all(root.path()).unwrap();
    }
}
