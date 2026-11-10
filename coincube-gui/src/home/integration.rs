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
