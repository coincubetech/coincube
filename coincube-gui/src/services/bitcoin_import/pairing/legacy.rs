//! Existing target journals remain authoritative for published deposit addresses.
//! Never redirect an unfinished same-source Claim/Split into a new target.
use std::{
    fs::{self, File, OpenOptions},
    path::{Component, Path},
};

use coincube_core::foreign_split::SplitSource;
use fs4::fs_std::FileExt;

use crate::{
    app::settings::{CubeSettings, Settings, WalletId, SETTINGS_FILE_NAME},
    chain::ChainId,
    dir::CoincubeDirectory,
    services::claim_workflow::{self, SplitJournalSummary},
};

const RESUME_EXISTING: &str = "Another Claim or Split for this source wallet is still recorded, or its records cannot be safely checked. Resume the existing claim in its destination Cube before starting another. No previous records were changed.";

fn refused<T>() -> Result<T, String> {
    Err(RESUME_EXISTING.into())
}

fn exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => refused(),
    }
}

fn cube_wallet(cube: &CubeSettings) -> Option<&WalletId> {
    cube.vault_wallet_id.as_ref().or_else(|| {
        cube.bitcoin_import
            .as_ref()?
            .attachment()
            .map(|wallet| &wallet.wallet_id)
    })
}

fn safe_wallet_path(wallet: &WalletId) -> bool {
    let name = wallet.to_string();
    !name.is_empty()
        && name.len() <= 256
        && !name.contains('\\')
        && matches!(
            Path::new(&name).components().collect::<Vec<_>>().as_slice(),
            [Component::Normal(_)]
        )
}

/// Match the existing completed-Split discovery policy. A tombstone, missing
/// descriptors alone, or an unrelated completion record never releases a source.
fn recorded_complete(summary: &SplitJournalSummary, cube: &CubeSettings) -> bool {
    summary.two_step
        && summary.target_cube == cube.id
        && summary.descriptors_forgotten
        && !summary.step1_conflict
        && !summary.step2_dead_end
        && summary.step2_txid.is_some_and(|txid| {
            cube.split_from.iter().any(|record| {
                record.descriptor_digest == summary.source_digest && record.step2_txid == txid
            })
        })
}

/// Inspect every known BTCB2 target's exact source-digest directory, including
/// funding-only records that ordinary intent discovery does not show. The
/// current target is exempt so its held journal lock can be requalified; its
/// identity and reservation are checked by that target's normal workflow.
/// Call under `with_source_reservation_lock` when creating a reservation.
pub(crate) fn refuse_conflicting_claim(
    root: &CoincubeDirectory,
    source: &SplitSource,
    target_cube: &str,
    target_wallet: &WalletId,
) -> Result<(), String> {
    let network = root.network_directory(ChainId::BitcoinBlake2b);
    if !exists(&network.path().join(SETTINGS_FILE_NAME))? {
        return Ok(());
    }
    let saved = Settings::from_file(&network).map_err(|_| RESUME_EXISTING.to_string())?;
    if !safe_wallet_path(target_wallet) {
        return refused();
    }
    let intended = network.coincubed_data_directory(target_wallet);
    let mut wallets = Vec::new();
    for wallet in saved
        .cubes
        .iter()
        .filter_map(cube_wallet)
        .cloned()
        .chain(saved.wallets.iter().map(|wallet| wallet.wallet_id()))
    {
        if !wallets.contains(&wallet) {
            wallets.push(wallet);
        }
    }
    for wallet in wallets {
        if !safe_wallet_path(&wallet) {
            return refused();
        }
        let data = network.coincubed_data_directory(&wallet);
        if data.path() == intended.path() {
            if saved.cubes.iter().any(|cube| {
                cube_wallet(cube).is_some_and(|id| {
                    network.coincubed_data_directory(id).path() == intended.path()
                        && cube.id != target_cube
                })
            }) {
                return refused();
            }
            continue;
        }
        let directory = data.path().join("split").join(source.digest().to_string());
        if !exists(&directory)? {
            continue;
        }
        if !fs::symlink_metadata(&directory)
            .map_err(|_| RESUME_EXISTING.to_string())?
            .is_dir()
        {
            return refused();
        }
        match claim_workflow::peek_split_journal(&directory)
            .map_err(|_| RESUME_EXISTING.to_string())?
        {
            Some(summary) => {
                let owners: Vec<_> = saved
                    .cubes
                    .iter()
                    .filter(|cube| {
                        cube.id == summary.target_cube && cube_wallet(cube) == Some(&wallet)
                    })
                    .collect();
                if summary.source_digest != source.digest()
                    || owners.len() != 1
                    || !recorded_complete(&summary, owners[0])
                {
                    return refused();
                }
            }
            None => {
                if let Some(reservation) = claim_workflow::peek_split_funding(&directory)
                    .map_err(|_| RESUME_EXISTING.to_string())?
                {
                    // The reader validates the descriptors against this digest.
                    // Even a not-yet-released reservation owns these indices;
                    // a released reservation may already have been shown unpaid.
                    let recorded = reservation
                        .source()
                        .map_err(|_| RESUME_EXISTING.to_string())?;
                    if recorded.external() != source.external()
                        || recorded.internal() != source.internal()
                        || reservation.source_digest != source.digest()
                    {
                        return refused();
                    }
                    return refused();
                }
            }
        }
    }
    Ok(())
}

/// Serialize the cross-target check and reservation publication for all new
/// standalone and paired Split starts using this exact public source. A stable
/// lock inode is retained forever; existing journal data is never migrated.
pub(crate) fn with_source_reservation_lock<T>(
    root: &CoincubeDirectory,
    source: &SplitSource,
    reserve: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let directory = root
        .network_directory(ChainId::BitcoinBlake2b)
        .path()
        .join("split-source-locks")
        .join(source.digest().to_string());
    claim_workflow::prepare_directory(&directory).map_err(|_| RESUME_EXISTING.to_string())?;
    let _lock = source_lock(&directory.join("reservation.lock"))?;
    reserve()
}

fn source_lock(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the reparse point itself and keep the lock from being deleted.
        options.custom_flags(0x0020_0000).share_mode(3);
    }
    let file = options
        .open(path)
        .map_err(|_| RESUME_EXISTING.to_string())?;
    let metadata = file.metadata().map_err(|_| RESUME_EXISTING.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return refused();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return refused();
        }
    }
    if !file
        .try_lock_exclusive()
        .map_err(|_| RESUME_EXISTING.to_string())?
    {
        return Err("Another Claim or Split is reserving addresses for this source wallet. Retry after it finishes.".into());
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::settings::SplitFromRecord,
        services::{
            claim_workflow::Context,
            foreign_scan::{Branch, BranchCoverage, ScanReport},
            foreign_split_inventory::SplitInventory,
        },
    };
    use coincube_core::miniscript::{
        bitcoin::{
            bip32::{Xpriv, Xpub},
            hashes::Hash,
            secp256k1::Secp256k1,
            BlockHash, Network, Txid,
        },
        Descriptor,
    };
    use std::str::FromStr;

    struct Fixture {
        root: CoincubeDirectory,
        legacy: CubeSettings,
        target: CubeSettings,
    }
    impl Fixture {
        async fn new() -> Self {
            let root = CoincubeDirectory::new(
                std::env::temp_dir()
                    .join(format!("legacy-claim-reservation-{}", uuid::Uuid::new_v4())),
            );
            let network = root.network_directory(ChainId::BitcoinBlake2b);
            network.init().unwrap();
            let mut legacy =
                CubeSettings::new("Synthetic old target".into(), ChainId::BitcoinBlake2b);
            legacy.vault_wallet_id = Some(WalletId {
                descriptor_checksum: "deadbeef".into(),
                timestamp: Some(11),
            });
            let mut target =
                CubeSettings::new("Synthetic paired target".into(), ChainId::BitcoinBlake2b);
            target.vault_wallet_id = Some(WalletId {
                descriptor_checksum: "cafebabe".into(),
                timestamp: Some(22),
            });
            crate::app::settings::update_settings_file(&network, |mut saved| {
                saved.cubes = vec![legacy.clone(), target.clone()];
                Some(saved)
            })
            .await
            .unwrap();
            Self {
                root,
                legacy,
                target,
            }
        }
        fn directory(&self, cube: &CubeSettings, source: &SplitSource) -> std::path::PathBuf {
            self.root
                .network_directory(ChainId::BitcoinBlake2b)
                .coincubed_data_directory(cube_wallet(cube).unwrap())
                .path()
                .join("split")
                .join(source.digest().to_string())
        }
        fn check(&self, source: &SplitSource, cube: &CubeSettings) -> Result<(), String> {
            refuse_conflicting_claim(&self.root, source, &cube.id, cube_wallet(cube).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.root.path());
        }
    }

    fn source(seed: u8) -> SplitSource {
        let secp = Secp256k1::new();
        let master = Xpriv::new_master(Network::Bitcoin, &[seed; 32]).unwrap();
        let key = Xpub::from_priv(&secp, &master);
        SplitSource::new(
            Descriptor::from_str(&format!("wpkh({key}/0/*)")).unwrap(),
            Some(Descriptor::from_str(&format!("wpkh({key}/1/*)")).unwrap()),
        )
        .unwrap()
    }

    fn inventory() -> SplitInventory {
        let report = |chain, byte| {
            ScanReport::for_test(chain, 1, BlockHash::from_byte_array([byte; 32]), Vec::new())
                .with_coverage(vec![BranchCoverage {
                    branch: Branch::External,
                    start: 0,
                    end_exclusive: 20,
                    last_used: Some(2),
                }])
        };
        SplitInventory::join(
            &report(ChainId::BitcoinBlake2b, 3).with_fork_height(Some(2)),
            &report(ChainId::Bitcoin, 4),
            1,
            true,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn unpaid_legacy_reservation_blocks_other_target_without_changing_old_data() {
        let f = Fixture::new().await;
        let source = source(42);
        let directory = f.directory(&f.legacy, &source);
        let saved = claim_workflow::reserve_split_funding(
            &directory,
            &source,
            &f.legacy.id,
            &Context {
                generation: 1,
                account: "synthetic-account".into(),
                provider: "https://synthetic.invalid".into(),
            },
            &inventory(),
        )
        .unwrap();
        // Neither an unreleased reservation nor one already displayed to the
        // user can be reclaimed merely because its deposit addresses are unpaid.
        let path = directory.join("split-funding.json");
        let before = fs::read(&path).unwrap();
        assert!(f
            .check(&source, &f.target)
            .unwrap_err()
            .contains("Resume the existing claim"));
        assert_eq!(fs::read(&path).unwrap(), before);
        let released = claim_workflow::release_split_funding(&directory, &saved).unwrap();
        assert!(released.is_released());
        let before = fs::read(&path).unwrap();
        assert!(f.check(&source, &f.target).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(f.check(&source, &f.legacy).is_ok()); // own current target resumes
        assert!(f.check(&self::source(43), &f.target).is_ok()); // another source is independent
    }

    #[tokio::test]
    async fn malformed_same_source_record_refuses_without_migration_or_deletion() {
        let f = Fixture::new().await;
        let source = source(44);
        let directory = f.directory(&f.legacy, &source);
        claim_workflow::prepare_directory(&directory).unwrap();
        let path = directory.join("intent.json");
        fs::write(&path, b"malformed synthetic old claim").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let before = fs::read(&path).unwrap();
        assert!(f.check(&source, &f.target).is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(f.check(&self::source(45), &f.target).is_ok());
    }

    #[test]
    fn completed_policy_requires_exact_recorded_source_target_and_step2() {
        let source = source(46);
        let txid = Txid::from_byte_array([8; 32]);
        let mut cube = CubeSettings::new("Completed old target".into(), ChainId::BitcoinBlake2b);
        let mut summary = SplitJournalSummary {
            two_step: true,
            source_digest: source.digest(),
            target_cube: cube.id.clone(),
            descriptors_forgotten: true,
            step2_txid: Some(txid),
            step1_conflict: false,
            step2_dead_end: false,
        };
        assert!(!recorded_complete(&summary, &cube));
        cube.split_from.push(SplitFromRecord {
            descriptor_digest: source.digest(),
            completed_height: 900,
            step2_txid: txid,
        });
        assert!(recorded_complete(&summary, &cube));
        summary.step1_conflict = true;
        assert!(!recorded_complete(&summary, &cube));
        summary.step1_conflict = false;
        summary.step2_dead_end = true;
        assert!(!recorded_complete(&summary, &cube));
        summary.step2_dead_end = false;
        summary.descriptors_forgotten = false;
        assert!(!recorded_complete(&summary, &cube));
        summary.descriptors_forgotten = true;
        summary.step2_txid = Some(Txid::from_byte_array([9; 32]));
        assert!(!recorded_complete(&summary, &cube));
        summary.step2_txid = Some(txid);
        summary.target_cube = "another-target".into();
        assert!(!recorded_complete(&summary, &cube));
    }

    #[test]
    fn source_lock_serializes_competing_targets_and_releases_on_return() {
        let root = CoincubeDirectory::new(
            std::env::temp_dir().join(format!("claim-source-lock-{}", uuid::Uuid::new_v4())),
        );
        let source = source(47);
        with_source_reservation_lock(&root, &source, || {
            assert!(with_source_reservation_lock(&root, &source, || Ok(())).is_err());
            assert!(with_source_reservation_lock(&root, &self::source(48), || Ok(())).is_ok());
            Ok(())
        })
        .unwrap();
        assert!(with_source_reservation_lock(&root, &source, || Ok(())).is_ok());
        fs::remove_dir_all(root.path()).unwrap();
    }
}
