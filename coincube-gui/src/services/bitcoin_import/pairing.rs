//! Dedicated imported-wallet fork pairing. The copied Cube seed is only the
//! unlock identity; all spending policy and external signers remain imported.
pub(crate) mod legacy;

use super::*;
use crate::{
    app::settings::{bitcoin_import::ImportedClaimPair, BitcoinImport, CubeSettings},
    chain::ChainId,
    dir::CoincubeDirectory,
    signer::Signer,
};
use sha2::{Digest, Sha256};

pub fn target_cube_id(source_cube_id: &str) -> String {
    let digest =
        Sha256::digest(format!("coincube/imported-btcb2-pair/v1/{source_cube_id}").as_bytes());
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes).to_string()
}

fn unique_cube<'a>(saved: &'a Settings, id: &str) -> Result<&'a CubeSettings, String> {
    let mut matches = saved.cubes.iter().filter(|c| c.id == id);
    let cube = matches
        .next()
        .ok_or("The paired Cube is no longer available.")?;
    if matches.next().is_some() {
        return Err("Duplicate Cube records prevent safe pairing.".into());
    }
    Ok(cube)
}

fn unique_wallet<'a>(saved: &'a Settings, id: &WalletId) -> Result<&'a WalletSettings, String> {
    let mut matches = saved.wallets.iter().filter(|w| &w.wallet_id() == id);
    let wallet = matches
        .next()
        .ok_or("Imported signer metadata is unavailable.")?;
    if matches.next().is_some() {
        return Err("Duplicate wallet records prevent safe pairing.".into());
    }
    Ok(wallet)
}

fn source_binding(source: &CubeSettings) -> Result<ImportedClaimPair, String> {
    if source.network != ChainId::Bitcoin
        || source.vault_wallet_id.is_some()
        || source.is_passkey_cube()
    {
        return Err(
            "Open a PIN-unlocked imported Bitcoin wallet to create its paired Claim Cube.".into(),
        );
    }
    let import = source
        .bitcoin_import
        .as_ref()
        .ok_or("The source imported wallet is missing.")?;
    let wallet = import
        .active_attachment()
        .ok_or("The source imported wallet is not installed.")?;
    wallet.validate(source.network)?;
    Ok(ImportedClaimPair {
        source_cube_id: source.id.clone(),
        source_operation_id: import.operation_id,
        source_wallet: wallet.clone(),
    })
}

fn paired_wallet(binding: &ImportedClaimPair) -> Result<ImportedWalletAttachment, String> {
    ImportedWalletAttachment::new(
        binding
            .source_wallet
            .descriptor
            .paired_blake2b()
            .map_err(|e| e.to_string())?,
        ChainId::BitcoinBlake2b,
        binding.source_wallet.birthday,
        binding.source_wallet.initial_lookahead,
    )
}

fn validate_target(source: &CubeSettings, target: &CubeSettings) -> Result<(), String> {
    let binding = source_binding(source)?;
    let wallet = paired_wallet(&binding)?;
    if target.id != target_cube_id(&source.id)
        || target.network != ChainId::BitcoinBlake2b
        || target.imported_claim_source.as_deref() != Some(&binding)
        || target.vault_wallet_id.is_some()
        || target.master_signer_fingerprint != source.master_signer_fingerprint
        || target.master_signer_fingerprint.is_none()
        || target.bitcoin_import.as_ref().and_then(|i| i.attachment()) != Some(&wallet)
    {
        return Err(
            "The paired Claim Cube conflicts with its imported source. No wallet was replaced."
                .into(),
        );
    }
    Ok(())
}

/// Validate a restored target without requiring its source to be present.
/// Spending/Claim handoff still requires validate_pair and both durable records.
pub fn validate_target_record(target: &CubeSettings) -> Result<(), String> {
    let binding = target
        .imported_claim_source
        .as_ref()
        .ok_or("Paired source metadata is unavailable.")?;
    binding.source_wallet.validate(ChainId::Bitcoin)?;
    let mut source = CubeSettings::new_with_raw_id(
        binding.source_cube_id.clone(),
        String::new(),
        ChainId::Bitcoin,
    );
    source.master_signer_fingerprint = target.master_signer_fingerprint;
    source.bitcoin_import = Some(Box::new(BitcoinImport {
        operation_id: binding.source_operation_id,
        stage: BitcoinImportStage::Ready {
            wallet: binding.source_wallet.clone(),
        },
        last_error: None,
    }));
    validate_target(&source, target)
}

fn matching_signers(source: &WalletSettings, target: &WalletSettings) -> bool {
    serde_json::to_value((
        &source.keys,
        &source.hardware_wallets,
        source.keychain_keys_recorded,
    ))
    .ok()
        == serde_json::to_value((
            &target.keys,
            &target.hardware_wallets,
            target.keychain_keys_recorded,
        ))
        .ok()
}

/// Recheck both durable records on every Claim handoff. No source mutation.
pub fn validate_pair(
    root: &CoincubeDirectory,
    target: &CubeSettings,
) -> Result<CubeSettings, String> {
    let binding = target
        .imported_claim_source
        .as_ref()
        .ok_or("This Cube is not a paired imported Claim destination.")?;
    let source_settings = Settings::from_file(&root.network_directory(ChainId::Bitcoin))
        .map_err(|e| e.to_string())?;
    let source = unique_cube(&source_settings, &binding.source_cube_id)?;
    validate_target(source, target)?;
    let target_settings =
        Settings::from_file(&root.network_directory(target.network)).map_err(|e| e.to_string())?;
    let saved_target = unique_cube(&target_settings, &target.id)?;
    validate_target(source, saved_target)?;
    let source_wallet = unique_wallet(&source_settings, &binding.source_wallet.wallet_id)?;
    let wallet = paired_wallet(binding)?;
    let target_wallet = unique_wallet(&target_settings, &wallet.wallet_id)?;
    if !matching_signers(source_wallet, target_wallet) {
        return Err("The paired imported wallet signer metadata changed.".into());
    }
    Ok(source.clone())
}

/// Recover a complete scan range from both the immutable paired identity and
/// the live source database. The saved minimum prevents an older restored
/// source snapshot from shrinking a Claim that has already been accepted.
pub(crate) fn claim_scan_state(
    root: &CoincubeDirectory,
    target_cube: &str,
    source_policy: &coincube_core::foreign_split::SplitSource,
    minimum_window: u32,
) -> Result<ClaimScanState, String> {
    if !(1..=10_000).contains(&minimum_window) {
        return Err("The imported Claim scan range is unavailable. Start Claim again.".into());
    }
    let saved = Settings::from_file(&root.network_directory(ChainId::BitcoinBlake2b))
        .map_err(|e| e.to_string())?;
    let target = unique_cube(&saved, target_cube)?;
    let target_wallet = target
        .bitcoin_import
        .as_ref()
        .and_then(|import| import.attachment())
        .ok_or("The paired imported target is unavailable.")?;
    legacy::refuse_conflicting_claim(root, source_policy, target_cube, &target_wallet.wallet_id)?;
    let source = validate_pair(root, target)?;
    let attachment = source
        .bitcoin_import
        .as_ref()
        .and_then(|import| import.active_attachment())
        .ok_or("The imported Claim source is unavailable.")?;
    if attachment.descriptor.receive_descriptor() != source_policy.external()
        || Some(attachment.descriptor.change_descriptor()) != source_policy.internal()
    {
        return Err("The imported Claim source descriptors changed.".into());
    }
    let state = paired_allocation_state(root, &source, attachment)?;
    let target_state = paired_allocation_state(root, target, target_wallet)?;
    let receive_allocation_floor = state.receive_index.max(target_state.receive_index);
    let current = [state, target_state]
        .iter()
        .try_fold(0_u32, |window, state| {
            Ok::<_, String>(
                window
                    .max(
                        state
                            .receive_index
                            .checked_add(1)
                            .ok_or("The imported address range overflowed.")?,
                    )
                    .max(
                        state
                            .change_index
                            .checked_add(1)
                            .ok_or("The imported address range overflowed.")?,
                    )
                    .max(state.discovery_target)
                    .max(state.receive_scanned)
                    .max(state.change_scanned),
            )
        })?;
    if current > 10_000 {
        return Err("The source address window exceeds the supported Claim range.".into());
    }
    let window = current.saturating_add(4).min(10_000).max(minimum_window);
    if window > 10_000 {
        return Err("The source address window exceeds the supported Claim range.".into());
    }
    Ok(ClaimScanState {
        source: crate::imported_claim::ImportedClaimSource {
            cube_id: source.id,
            name: source.name,
            external: attachment.descriptor.receive_descriptor().to_string(),
            internal: attachment.descriptor.change_descriptor().to_string(),
            address_window: window,
        },
        receive_allocation_floor,
    })
}

pub(crate) struct ClaimScanState {
    pub source: crate::imported_claim::ImportedClaimSource,
    pub receive_allocation_floor: u32,
}

pub(crate) fn claim_scan_source(
    root: &CoincubeDirectory,
    target_cube: &str,
    source_policy: &coincube_core::foreign_split::SplitSource,
    minimum_window: u32,
) -> Result<crate::imported_claim::ImportedClaimSource, String> {
    claim_scan_state(root, target_cube, source_policy, minimum_window).map(|state| state.source)
}

/// Before first pairing there is no target database. An existing target's
/// independent allocation must fit the initial Claim scan too.
pub(crate) fn existing_claim_window(
    root: &CoincubeDirectory,
    source_cube: &str,
    source_policy: &coincube_core::foreign_split::SplitSource,
    minimum: u32,
) -> Result<u32, String> {
    let directory = root.network_directory(ChainId::BitcoinBlake2b);
    if !directory.path().join(settings::SETTINGS_FILE_NAME).exists() {
        return Ok(minimum);
    }
    let saved = Settings::from_file(&directory).map_err(|error| error.to_string())?;
    let target = target_cube_id(source_cube);
    if !saved.cubes.iter().any(|cube| cube.id == target) {
        return Ok(minimum);
    }
    claim_scan_source(root, &target, source_policy, minimum).map(|source| source.address_window)
}

fn paired_allocation_state(
    root: &CoincubeDirectory,
    cube: &CubeSettings,
    wallet: &ImportedWalletAttachment,
) -> Result<coincubed::ImportedClaimState, String> {
    let data = root
        .network_directory(cube.network)
        .coincubed_data_directory(&wallet.wallet_id);
    let config =
        Config::from_file(Some(data.path().join(CONFIG_FILE))).map_err(|e| e.to_string())?;
    if config.main_descriptor != WalletDescriptor::Imported(wallet.descriptor.clone())
        || config.bitcoin_config.chain != cube.network
        || config.import_birthday != Some(wallet.birthday)
        || config.import_initial_lookahead != Some(wallet.initial_lookahead)
    {
        return Err("The imported Claim allocation identity changed.".into());
    }
    coincubed::read_imported_claim_state(&config, &data).map_err(|e| e.to_string())
}

/// Published initialization metadata is sufficient for a still-syncing pair:
/// do not reopen a database that its daemon may already have running.
fn validate_syncing_discovery(
    directory: &NetworkDirectory,
    target: &CubeSettings,
) -> Result<(), String> {
    let import = target
        .bitcoin_import
        .as_ref()
        .ok_or("The paired import is unavailable.")?;
    let missing = "The unfinished paired wallet has no verified copied discovery range. Finish or resolve its pending import before retrying Claim.";
    let journal = read_journal(&journal_path(directory, import.operation_id))?.ok_or(missing)?;
    if journal.cube_id != target.id
        || journal.operation_id != import.operation_id
        || Some(&journal.wallet) != import.active_attachment()
        || journal.restored_discovery_target.is_none()
        || !matches!(journal.phase, Phase::Published | Phase::Attached)
        || !owns_directory(&journal.final_path(directory), &journal)?
    {
        return Err(missing.into());
    }
    Ok(())
}

/// Create/retry the sole pair for this source, using the normal staged import
/// journal. Its separate identity never rewrites legacy Split attempts.
pub async fn install_pair(
    root: &CoincubeDirectory,
    source: &CubeSettings,
    signer: &Signer,
    pin: &str,
    backend: coincubed::config::BitcoinBackend,
) -> Result<CubeSettings, String> {
    let binding = source_binding(source)?;
    if source.master_signer_fingerprint != Some(signer.fingerprint()) || pin.is_empty() {
        return Err("Unlock the source Cube again before pairing.".into());
    }
    let source_dir = root.network_directory(source.network);
    let source_settings = Settings::from_file(&source_dir).map_err(|e| e.to_string())?;
    let current = unique_cube(&source_settings, &source.id)?;
    if source_binding(current)? != binding
        || current.master_signer_fingerprint != source.master_signer_fingerprint
        || current.backed_up != source.backed_up
    {
        return Err("The source Cube changed. Open Claim again.".into());
    }
    // A completed pair is reused without reopening or rewriting its database.
    // Source discovery may grow while the fork daemon is running.
    let target_dir = root.network_directory(ChainId::BitcoinBlake2b);
    if target_dir
        .path()
        .join(settings::SETTINGS_FILE_NAME)
        .exists()
    {
        let saved = Settings::from_file(&target_dir).map_err(|e| e.to_string())?;
        if saved
            .cubes
            .iter()
            .any(|c| c.id == target_cube_id(&source.id))
        {
            let prior = unique_cube(&saved, &target_cube_id(&source.id))?;
            validate_target(source, prior)?;
            if prior.has_imported_bitcoin() {
                validate_pair(root, prior)?;
                if !prior
                    .bitcoin_import
                    .as_ref()
                    .is_some_and(|import| import.is_ready())
                {
                    validate_syncing_discovery(&target_dir, prior)?;
                }
                return Ok(prior.clone());
            }
            if read_journal(&journal_path(
                &target_dir,
                prior
                    .bitcoin_import
                    .as_ref()
                    .ok_or("The paired import is unavailable.")?
                    .operation_id,
            ))?
            .is_some_and(|journal| journal.restored_discovery_target.is_none())
            {
                return Err("This unfinished paired wallet has no saved discovery range. Resolve its pending import before retrying Claim; no wallet data was changed.".into());
            }
        }
    }
    let source_data = source_dir.coincubed_data_directory(&binding.source_wallet.wallet_id);
    let source_config =
        Config::from_file(Some(source_data.path().join(CONFIG_FILE))).map_err(|e| e.to_string())?;
    if source_config.bitcoin_config.chain != source.network
        || source_config.main_descriptor.as_imported() != Some(&binding.source_wallet.descriptor)
        || source_config.import_birthday != Some(binding.source_wallet.birthday)
        || source_config.import_initial_lookahead != Some(binding.source_wallet.initial_lookahead)
    {
        return Err("The source wallet storage changed. Open Claim again.".into());
    }
    let address_state = coincubed::read_imported_claim_state(&source_config, &source_data)
        .map_err(|e| e.to_string())?;
    if !address_state.history_complete {
        return Err("Complete the source wallet scan before pairing.".into());
    }
    let discovery = address_state
        .discovery_target
        .max(address_state.receive_scanned)
        .max(address_state.change_scanned)
        .max(
            address_state
                .receive_index
                .checked_add(1)
                .ok_or("Source address range overflow")?,
        )
        .max(
            address_state
                .change_index
                .checked_add(1)
                .ok_or("Source address range overflow")?,
        );
    if discovery > 10_000 {
        return Err("Source discovery window exceeds the Claim limit.".into());
    }
    let source_metadata =
        unique_wallet(&source_settings, &binding.source_wallet.wallet_id)?.clone();
    let wallet = paired_wallet(&binding)?;
    let directory = root.network_directory(ChainId::BitcoinBlake2b);
    directory.init().map_err(|e| e.to_string())?;
    check_duress(&directory)?;
    let mut target = CubeSettings::new_with_raw_id(
        target_cube_id(&source.id),
        format!("{} · BTCB2", source.name),
        ChainId::BitcoinBlake2b,
    );
    target.master_signer_fingerprint = source.master_signer_fingerprint;
    target.backed_up = source.backed_up;
    target.mfa_done = source.mfa_done;
    target.imported_claim_source = Some(Box::new(binding));
    target.bitcoin_import = Some(Box::new(BitcoinImport {
        operation_id: Uuid::parse_str(&target.id).map_err(|e| e.to_string())?,
        stage: BitcoinImportStage::Installing {
            wallet: wallet.clone(),
        },
        last_error: None,
    }));
    let prior = read_journal(&journal_path(
        &directory,
        target.bitcoin_import.as_ref().unwrap().operation_id,
    ))?;
    if prior
        .as_ref()
        .is_some_and(|journal| journal.restored_discovery_target.is_none())
    {
        return Err("This unfinished paired wallet has no saved discovery range. Resolve its pending import before retrying Claim; no wallet data was changed.".into());
    }
    settings::update_settings_file_checked(&directory, |mut saved| {
        if saved.cubes.iter().any(|c|c.id == target.id) { validate_target(source,unique_cube(&saved,&target.id).map_err(SettingsError::WritingFile)?).map_err(SettingsError::WritingFile)?; }
        else {
            if saved.cubes.iter().any(|c|c.bitcoin_import.as_ref().and_then(|i|i.attachment()).is_some_and(|w|w.wallet_id == wallet.wallet_id)) { return Err(SettingsError::WritingFile("This imported fork wallet already belongs to another Cube. Resolve that attachment first.".into())); }
            saved.cubes.push(target.clone());
        }
        let mut metadata = source_metadata.clone();
        metadata.descriptor_checksum = wallet.wallet_id.descriptor_checksum.clone();
        metadata.pinned_at = wallet.wallet_id.timestamp;
        if let Some(prior) = saved.wallets.iter().find(|w|w.wallet_id() == wallet.wallet_id) {
            unique_wallet(&saved,&wallet.wallet_id).map_err(SettingsError::WritingFile)?;
            if !matching_signers(&metadata,prior) { return Err(SettingsError::WritingFile("Paired signer metadata conflicts; nothing was replaced.".into())); }
        } else { saved.wallets.push(metadata); }
        Ok(Some(saved))
    }).await.map_err(|e|e.to_string())?;
    let secret = crate::services::unlock::device_secret::get_or_create(root.path(), &target.id)
        .map_err(|e| e.to_string())?;
    crate::installer::persist_cube_master_seed(
        signer,
        root,
        ChainId::BitcoinBlake2b,
        pin,
        &target.id,
        Some(&secret),
    )
    .map_err(|e| e.to_string())?;
    crate::installer::ensure_gui_config(&directory, false).map_err(|e| e.to_string())?;
    let mut config = Config::new(
        coincubed::config::BitcoinConfig::new(target.network, std::time::Duration::from_secs(10)),
        Some(backend),
        log::LevelFilter::Info,
        WalletDescriptor::Imported(wallet.descriptor.clone()),
        directory.coincubed_data_directory(&wallet.wallet_id),
    );
    config.import_birthday = Some(wallet.birthday);
    config.import_initial_lookahead = Some(wallet.initial_lookahead);
    let restore_state = prior
        .as_ref()
        .and_then(|journal| journal.restored_address_state)
        .unwrap_or(RestoredAddressState {
            receive_index: address_state.receive_index,
            change_index: address_state.change_index,
        });
    let discovery = prior
        .and_then(|journal| journal.restored_discovery_target)
        .unwrap_or(discovery);
    install_restored_with_discovery(
        &directory,
        &target.id,
        target.bitcoin_import.as_ref().unwrap().operation_id,
        wallet.clone(),
        config,
        restore_state,
        discovery,
    )
    .await?;
    target = Settings::from_file(&directory)
        .map_err(|e| e.to_string())?
        .cubes
        .into_iter()
        .find(|c| c.id == target.id)
        .ok_or("Paired Cube disappeared.")?;
    validate_pair(root, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pair() -> (CubeSettings, CubeSettings) {
        let (mut source, _, _) = crate::imported_claim::tests::fixture();
        source.master_signer_fingerprint = Some([1, 2, 3, 4].into());
        source.backed_up = true;
        let binding = source_binding(&source).unwrap();
        let mut target = CubeSettings::new_with_raw_id(
            target_cube_id(&source.id),
            "Paired".into(),
            ChainId::BitcoinBlake2b,
        );
        target.master_signer_fingerprint = source.master_signer_fingerprint;
        target.backed_up = source.backed_up;
        target.bitcoin_import = Some(Box::new(BitcoinImport {
            operation_id: Uuid::new_v4(),
            stage: BitcoinImportStage::Ready {
                wallet: paired_wallet(&binding).unwrap(),
            },
            last_error: None,
        }));
        target.imported_claim_source = Some(Box::new(binding));
        (source, target)
    }
    fn prepare_allocation_storage(root: &CoincubeDirectory, cube: &CubeSettings, receive: u32) {
        let wallet = cube
            .bitcoin_import
            .as_ref()
            .unwrap()
            .active_attachment()
            .unwrap();
        let data = root
            .network_directory(cube.network)
            .coincubed_data_directory(&wallet.wallet_id);
        let staged = DataDirectory::new(data.path().with_extension("test-staging"));
        let mut config = Config::new(
            coincubed::config::BitcoinConfig::new(cube.network, std::time::Duration::from_secs(10)),
            Some(coincubed::config::BitcoinBackend::Electrum(
                coincubed::config::ElectrumConfig {
                    addr: "tcp://127.0.0.1:50001".into(),
                    validate_domain: true,
                },
            )),
            log::LevelFilter::Off,
            WalletDescriptor::Imported(wallet.descriptor.clone()),
            DataDirectory::new(data.path().to_path_buf()),
        );
        config.import_birthday = Some(wallet.birthday);
        config.import_initial_lookahead = Some(wallet.initial_lookahead);
        coincubed::create_imported_storage(
            &config,
            &staged,
            wallet.birthday,
            wallet.initial_lookahead,
        )
        .unwrap();
        coincubed::restore_imported_address_state(&config, &staged, receive, 0).unwrap();
        std::fs::rename(staged.path(), data.path()).unwrap();
        coincubed::complete_imported_history_for_test(
            &config,
            &data,
            receive + wallet.initial_lookahead,
            wallet.initial_lookahead,
        )
        .unwrap();
    }

    #[test]
    fn exact_pair_keeps_branches_and_external_origins_but_separates_identity() {
        let (source, target) = pair();
        validate_target(&source, &target).unwrap();
        let a = source
            .bitcoin_import
            .as_ref()
            .unwrap()
            .attachment()
            .unwrap();
        let b = target
            .bitcoin_import
            .as_ref()
            .unwrap()
            .attachment()
            .unwrap();
        assert_eq!(
            a.descriptor.receive_descriptor(),
            b.descriptor.receive_descriptor()
        );
        assert_eq!(
            a.descriptor.change_descriptor(),
            b.descriptor.change_descriptor()
        );
        assert_ne!(a.wallet_id, b.wallet_id);
        assert_ne!(target.id, source.id);
        assert_eq!(target.id, target_cube_id(&source.id));
        assert!(target.spark_stable_balance.is_none());
    }
    #[test]
    fn pairing_rejects_changed_source_operation_target_or_unlock_identity() {
        let (source, target) = pair();
        let mut wrong = source.clone();
        wrong.bitcoin_import.as_mut().unwrap().operation_id = Uuid::new_v4();
        assert!(validate_target(&wrong, &target).is_err());
        let mut wrong = target.clone();
        wrong.id = Uuid::new_v4().to_string();
        assert!(validate_target(&source, &wrong).is_err());
        let mut wrong = target.clone();
        wrong.master_signer_fingerprint = Some([9, 9, 9, 9].into());
        assert!(validate_target(&source, &wrong).is_err());
        let mut wrong = target;
        wrong
            .imported_claim_source
            .as_mut()
            .unwrap()
            .source_wallet
            .birthday += 1;
        assert!(validate_target(&source, &wrong).is_err());
    }
    #[test]
    fn duplicates_are_refused_instead_of_selecting_first() {
        let (source, _) = pair();
        let saved = Settings {
            cubes: vec![source.clone(), source.clone()],
            ..Default::default()
        };
        assert!(unique_cube(&saved, &source.id).is_err());
    }
    #[test]
    fn imported_claim_scan_range_survives_restart_and_revalidates_source() {
        let (source, target) = pair();
        let root = CoincubeDirectory::new(
            std::env::temp_dir().join(format!("pair-scan-source-{}", Uuid::new_v4(),)),
        );
        for cube in [&source, &target] {
            let attachment = cube.bitcoin_import.as_ref().unwrap().attachment().unwrap();
            let directory = root.network_directory(cube.network);
            directory.init().unwrap();
            let metadata = WalletSettings {
                name: "Synthetic signer".into(),
                alias: None,
                descriptor_checksum: attachment.wallet_id.descriptor_checksum.clone(),
                pinned_at: attachment.wallet_id.timestamp,
                keys: Vec::new(),
                hardware_wallets: Vec::new(),
                remote_backend_auth: None,
                start_internal_bitcoind: Some(false),
                pending_rescan: None,
                keychain_keys_recorded: false,
            };
            std::fs::write(
                directory.path().join(settings::SETTINGS_FILE_NAME),
                serde_json::to_vec(&Settings {
                    cubes: vec![cube.clone()],
                    wallets: vec![metadata],
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        }
        prepare_allocation_storage(&root, &source, 40);
        prepare_allocation_storage(&root, &target, 50);
        let attachment = source
            .bitcoin_import
            .as_ref()
            .unwrap()
            .attachment()
            .unwrap();
        let source_policy = coincube_core::foreign_split::SplitSource::new(
            attachment.descriptor.receive_descriptor().clone(),
            Some(attachment.descriptor.change_descriptor().clone()),
        )
        .unwrap();
        let (_, _, client) = crate::imported_claim::tests::fixture();
        let validate = |minimum| {
            let scan_source = claim_scan_source(&root, &target.id, &source_policy, minimum)?;
            crate::imported_claim::ImportedClaimRequest::new(scan_source.clone(), &client)?
                .validate(&root, &client)?;
            Ok::<_, String>(scan_source.address_window)
        };
        let allocation = claim_scan_state(&root, &target.id, &source_policy, 1).unwrap();
        assert_eq!(allocation.receive_allocation_floor, 50);
        assert_eq!(allocation.source.address_window, 254);
        assert_eq!(
            existing_claim_window(&root, &source.id, &source_policy, 1).unwrap(),
            254
        );
        let target_wallet = target
            .bitcoin_import
            .as_ref()
            .unwrap()
            .active_attachment()
            .unwrap();
        let target_data = root
            .network_directory(target.network)
            .coincubed_data_directory(&target_wallet.wallet_id);
        let staged = DataDirectory::new(target_data.path().with_extension("test-staging"));
        let target_config = Config::from_file(Some(target_data.path().join(CONFIG_FILE))).unwrap();
        std::fs::rename(target_data.path(), staged.path()).unwrap();
        coincubed::restore_imported_address_state(&target_config, &staged, 750, 0).unwrap();
        std::fs::rename(staged.path(), target_data.path()).unwrap();
        let allocation = claim_scan_state(&root, &target.id, &source_policy, 1).unwrap();
        assert_eq!(allocation.receive_allocation_floor, 750);
        assert_eq!(allocation.source.address_window, 755);
        assert_eq!(validate(444).unwrap(), 755);
        assert_eq!(validate(10_000).unwrap(), 10_000);
        let data = root
            .network_directory(ChainId::Bitcoin)
            .coincubed_data_directory(&attachment.wallet_id);
        let config = Config::from_file(Some(data.path().join(CONFIG_FILE))).unwrap();
        coincubed::expand_imported_discovery(&config, &data, 650).unwrap();
        assert!(validate(444).is_err());
        coincubed::complete_imported_history_for_test(&config, &data, 690, 650).unwrap();
        assert_eq!(validate(444).unwrap(), 755);
        assert_eq!(validate(900).unwrap(), 900); // durable minimum cannot shrink

        let directory = root.network_directory(ChainId::Bitcoin);
        let path = directory.path().join(settings::SETTINGS_FILE_NAME);
        let original = std::fs::read(&path).unwrap();
        let mut changed = Settings::from_file(&directory).unwrap();
        changed.cubes[0]
            .bitcoin_import
            .as_mut()
            .unwrap()
            .operation_id = Uuid::new_v4();
        std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(validate(900).is_err());
        std::fs::write(&path, original).unwrap();
        assert_eq!(validate(900).unwrap(), 900);
        std::fs::remove_file(&path).unwrap();
        assert!(validate(900).is_err());
        std::fs::remove_dir_all(root.path()).unwrap();
    }

    #[tokio::test]
    async fn interrupted_pair_copies_manual_discovery_before_attachment_and_reuses_syncing_without_sqlite(
    ) {
        let (mut source, mut target) = pair();
        let signer =
            Signer::generate(coincube_core::miniscript::bitcoin::Network::Bitcoin).unwrap();
        source.master_signer_fingerprint = Some(signer.fingerprint());
        source.bitcoin_import.as_mut().unwrap().stage = BitcoinImportStage::Ready {
            wallet: ImportedWalletAttachment::new(
                source
                    .bitcoin_import
                    .as_ref()
                    .unwrap()
                    .attachment()
                    .unwrap()
                    .descriptor
                    .clone(),
                ChainId::Bitcoin,
                0,
                20,
            )
            .unwrap(),
        };
        target.master_signer_fingerprint = source.master_signer_fingerprint;
        let binding = source_binding(&source).unwrap();
        let wallet = paired_wallet(&binding).unwrap();
        target.imported_claim_source = Some(Box::new(binding));
        let operation_id = Uuid::parse_str(&target.id).unwrap();
        target.bitcoin_import = Some(Box::new(BitcoinImport {
            operation_id,
            stage: BitcoinImportStage::Installing {
                wallet: wallet.clone(),
            },
            last_error: None,
        }));
        let root = CoincubeDirectory::new(
            std::env::temp_dir().join(format!("pair-discovery-{}", Uuid::new_v4())),
        );
        for cube in [&source, &target] {
            let attachment = cube.bitcoin_import.as_ref().unwrap().attachment().unwrap();
            let directory = root.network_directory(cube.network);
            directory.init().unwrap();
            let metadata = WalletSettings {
                name: "Synthetic external signer".into(),
                alias: None,
                descriptor_checksum: attachment.wallet_id.descriptor_checksum.clone(),
                pinned_at: attachment.wallet_id.timestamp,
                keys: Vec::new(),
                hardware_wallets: Vec::new(),
                remote_backend_auth: None,
                start_internal_bitcoind: Some(false),
                pending_rescan: None,
                keychain_keys_recorded: false,
            };
            std::fs::write(
                directory.path().join(settings::SETTINGS_FILE_NAME),
                serde_json::to_vec(&Settings {
                    cubes: vec![cube.clone()],
                    wallets: vec![metadata],
                    ..Default::default()
                })
                .unwrap(),
            )
            .unwrap();
        }
        crate::imported_claim::tests::prepare_storage(&root, &source);
        let source_wallet = source
            .bitcoin_import
            .as_ref()
            .unwrap()
            .attachment()
            .unwrap();
        let source_data = root
            .network_directory(ChainId::Bitcoin)
            .coincubed_data_directory(&source_wallet.wallet_id);
        let source_config = Config::from_file(Some(source_data.path().join(CONFIG_FILE))).unwrap();
        coincubed::expand_imported_discovery(&source_config, &source_data, 650).unwrap();
        coincubed::complete_imported_history_for_test(&source_config, &source_data, 650, 650)
            .unwrap();
        let state = coincubed::read_imported_claim_state(&source_config, &source_data).unwrap();
        assert_eq!(
            (
                state.receive_index,
                state.change_index,
                state.discovery_target
            ),
            (0, 0, 650)
        );
        let directory = root.network_directory(ChainId::BitcoinBlake2b);
        let data = directory.coincubed_data_directory(&wallet.wallet_id);
        let backend =
            coincubed::config::BitcoinBackend::Electrum(coincubed::config::ElectrumConfig {
                addr: "tcp://127.0.0.1:50001".into(),
                validate_domain: true,
            });
        let mut config = Config::new(
            coincubed::config::BitcoinConfig::new(
                target.network,
                std::time::Duration::from_secs(10),
            ),
            Some(backend.clone()),
            log::LevelFilter::Off,
            WalletDescriptor::Imported(wallet.descriptor.clone()),
            DataDirectory::new(data.path().to_path_buf()),
        );
        config.import_birthday = Some(0);
        config.import_initial_lookahead = Some(20);
        let address_state = RestoredAddressState {
            receive_index: 0,
            change_index: 0,
        };
        let mut journal = Journal {
            version: 1,
            cube_id: target.id.clone(),
            operation_id,
            wallet: wallet.clone(),
            phase: Phase::Prepared,
            restored_address_state: Some(address_state),
            restored_discovery_target: Some(650),
        };
        journal.persist(&directory).unwrap();
        let staged = DataDirectory::new(journal.stage_path(&directory));
        staged.init().unwrap();
        write_json(&staged.path().join(OWNER_FILE), &journal).unwrap();
        coincubed::create_imported_storage(&config, &staged, 0, 20).unwrap();
        // Model interruption before discovery initialization: only 20 exists,
        // but the unpublished operation already durably requires 650.
        assert!(!data.path().exists());
        assert!(coincubed::restore_imported_discovery_target(&config, &data, 650).is_err());
        assert!(!data.path().exists());
        install_restored_with_discovery(
            &directory,
            &target.id,
            operation_id,
            wallet.clone(),
            config.clone(),
            address_state,
            650,
        )
        .await
        .unwrap();
        let saved_config = Config::from_file(Some(data.path().join(CONFIG_FILE))).unwrap();
        let state = coincubed::read_imported_claim_state(&saved_config, &data).unwrap();
        assert_eq!(
            (
                state.receive_index,
                state.change_index,
                state.discovery_target
            ),
            (0, 0, 650)
        );
        assert!(!state.history_complete);
        assert_eq!(saved_config.import_initial_lookahead, Some(20));
        journal = read_journal(&journal.path(&directory)).unwrap().unwrap();
        assert_eq!(journal.restored_discovery_target, Some(650));
        assert!(owns_directory(data.path(), &journal).unwrap());

        // Model interruption after full staged initialization but before rename.
        std::fs::rename(data.path(), staged.path()).unwrap();
        journal.phase = Phase::StorageReady;
        journal.persist(&directory).unwrap();
        settings::update_settings_file(&directory, |mut saved| {
            saved.cubes[0].bitcoin_import.as_mut().unwrap().stage =
                BitcoinImportStage::Installing {
                    wallet: wallet.clone(),
                };
            Some(saved)
        })
        .await
        .unwrap();
        resume(&directory, &target.id, operation_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            coincubed::read_imported_claim_state(&saved_config, &data)
                .unwrap()
                .discovery_target,
            650
        );

        // The address contract cannot be changed by a retry.
        assert!(install_restored_with_discovery(
            &directory,
            &target.id,
            operation_id,
            wallet.clone(),
            config,
            address_state,
            651
        )
        .await
        .is_err());
        // Model interruption after attachment but before the final journal write.
        journal.phase = Phase::Published;
        journal.persist(&directory).unwrap();
        let before = std::fs::read(data.sqlite_db_file_path()).unwrap();
        std::fs::write(
            data.path().join("coincubed.sqlite3-wal"),
            b"synthetic-running-target",
        )
        .unwrap();
        std::fs::remove_dir_all(source_data.path()).unwrap();
        for _ in 0..2 {
            let reused = install_pair(&root, &source, &signer, "1234", backend.clone())
                .await
                .unwrap();
            assert!(matches!(
                reused.bitcoin_import.unwrap().stage,
                BitcoinImportStage::Syncing { .. }
            ));
        }
        assert_eq!(std::fs::read(data.sqlite_db_file_path()).unwrap(), before);
        assert_eq!(
            std::fs::read(data.path().join("coincubed.sqlite3-wal")).unwrap(),
            b"synthetic-running-target"
        );
        // An old pending pair with no discovery contract cannot silently skip it.
        journal.restored_discovery_target = None;
        journal.persist(&directory).unwrap();
        write_json(&data.path().join(OWNER_FILE), &journal).unwrap();
        assert!(
            install_pair(&root, &source, &signer, "1234", backend.clone())
                .await
                .unwrap_err()
                .contains("copied discovery range")
        );
        settings::update_settings_file(&directory, |mut saved| {
            saved.cubes[0].bitcoin_import.as_mut().unwrap().stage =
                BitcoinImportStage::Installing {
                    wallet: wallet.clone(),
                };
            Some(saved)
        })
        .await
        .unwrap();
        assert!(install_pair(&root, &source, &signer, "1234", backend)
            .await
            .unwrap_err()
            .contains("no saved discovery range"));
        assert_eq!(std::fs::read(data.sqlite_db_file_path()).unwrap(), before);
        assert_eq!(
            std::fs::read(data.path().join("coincubed.sqlite3-wal")).unwrap(),
            b"synthetic-running-target"
        );
        std::fs::remove_dir_all(root.path()).unwrap();
    }

    #[tokio::test]
    async fn ready_pair_reuses_without_opening_running_database_or_reinstalling_snapshot() {
        let (mut source, mut target) = pair();
        let signer =
            Signer::generate(coincube_core::miniscript::bitcoin::Network::Bitcoin).unwrap();
        source.master_signer_fingerprint = Some(signer.fingerprint());
        target.master_signer_fingerprint = source.master_signer_fingerprint;
        let root = CoincubeDirectory::new(
            std::env::temp_dir().join(format!("pair-reuse-{}", Uuid::new_v4())),
        );
        let source_wallet = source
            .bitcoin_import
            .as_ref()
            .unwrap()
            .attachment()
            .unwrap();
        let target_wallet = target
            .bitcoin_import
            .as_ref()
            .unwrap()
            .attachment()
            .unwrap();
        let mut metadata = WalletSettings {
            name: "Synthetic external signer".into(),
            alias: None,
            descriptor_checksum: source_wallet.wallet_id.descriptor_checksum.clone(),
            pinned_at: None,
            keys: Vec::new(),
            hardware_wallets: Vec::new(),
            remote_backend_auth: None,
            start_internal_bitcoind: Some(false),
            pending_rescan: None,
            keychain_keys_recorded: false,
        };
        let source_dir = root.network_directory(ChainId::Bitcoin);
        source_dir.init().unwrap();
        settings::update_settings_file(&source_dir, |mut s| {
            s.cubes.push(source.clone());
            s.wallets.push(metadata.clone());
            Some(s)
        })
        .await
        .unwrap();
        metadata.descriptor_checksum = target_wallet.wallet_id.descriptor_checksum.clone();
        let target_dir = root.network_directory(ChainId::BitcoinBlake2b);
        target_dir.init().unwrap();
        settings::update_settings_file(&target_dir, |mut s| {
            s.cubes.push(target.clone());
            s.wallets.push(metadata);
            Some(s)
        })
        .await
        .unwrap();
        // A running target may have WAL sidecars; a growing source may also
        // be unavailable for preflight. Reuse needs neither database.
        let data = target_dir.coincubed_data_directory(&target_wallet.wallet_id);
        std::fs::create_dir_all(data.path()).unwrap();
        std::fs::write(
            data.path().join("coincubed.sqlite3-wal"),
            b"synthetic-running-database",
        )
        .unwrap();
        let backend =
            coincubed::config::BitcoinBackend::Electrum(coincubed::config::ElectrumConfig {
                addr: "tcp://127.0.0.1:50001".into(),
                validate_domain: true,
            });
        for _ in 0..2 {
            let reused = install_pair(&root, &source, &signer, "1234", backend.clone())
                .await
                .unwrap();
            assert_eq!(reused.id, target.id);
            assert!(reused.bitcoin_import.unwrap().is_ready());
        }
        assert_eq!(
            std::fs::read(data.path().join("coincubed.sqlite3-wal")).unwrap(),
            b"synthetic-running-database"
        );
        std::fs::remove_dir_all(root.path()).unwrap();
    }
}
