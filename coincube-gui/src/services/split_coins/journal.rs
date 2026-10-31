//! The Vault's split records, next to its daemon data. Display state only:
//! a record says which transaction this Cube built as a split, never that it
//! may be signed or broadcast.

use std::{
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::SplitRecord;
use crate::{chain::ChainId, dir::CoincubeDirectory};

const FILE: &str = "splits.json";
const VERSION: u32 = 1;
/// Far more records than a Vault can accumulate inside their lifetime.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    splits: Vec<SplitRecord>,
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

/// Records for a Vault on `chain`. A missing, oversized or unreadable file,
/// and records for another chain, read as none.
pub fn load(data_dir: &Path, chain: ChainId) -> Vec<SplitRecord> {
    let file = path(data_dir);
    if std::fs::metadata(&file).map_or(true, |m| m.len() > MAX_BYTES) {
        return Vec::new();
    }
    match std::fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Record>(&bytes).ok())
    {
        Some(record) if record.version == VERSION => record
            .splits
            .into_iter()
            .filter(|split| split.chain == chain)
            .collect(),
        _ => {
            tracing::warn!("ignoring unreadable {}", file.display());
            Vec::new()
        }
    }
}

pub fn save(data_dir: &Path, splits: &[SplitRecord]) -> io::Result<()> {
    let bytes = serde_json::to_vec(&Record {
        version: VERSION,
        splits: splits.to_vec(),
    })
    .map_err(io::Error::other)?;
    crate::services::chain_status::store::write_atomically(&path(data_dir), &bytes)
}

/// The data directory of the twin Cube's Vault: the Cube on the twin chain
/// whose Vault has the same descriptor. `None` unless exactly one matches.
pub fn twin_data_dir(
    datadir: &CoincubeDirectory,
    chain: ChainId,
    descriptor_checksum: &str,
) -> Option<PathBuf> {
    let twin = crate::services::entangled::twin_chain(chain)?;
    let network = datadir.network_directory(twin);
    if !network
        .path()
        .join(crate::app::settings::SETTINGS_FILE_NAME)
        .is_file()
    {
        return None;
    }
    let settings = crate::app::settings::Settings::from_file(&network).ok()?;
    let mut matches = settings.cubes.iter().filter_map(|cube| {
        cube.vault_wallet_id
            .as_ref()
            .filter(|id| cube.network == twin && id.descriptor_checksum == descriptor_checksum)
    });
    let wallet = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(
        network
            .coincubed_data_directory(wallet)
            .path()
            .to_path_buf(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::split_coins::Method;
    use coincube_core::miniscript::bitcoin::{OutPoint, Txid};
    use std::str::FromStr;

    #[test]
    fn round_trips_and_filters_by_chain() {
        let dir = std::env::temp_dir().join(format!("coincube-splits-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let txid = Txid::from_str(&"ab".repeat(32)).unwrap();
        let record = |chain| SplitRecord {
            txid,
            chain,
            method: Method::SplitKey,
            inputs: vec![OutPoint::new(txid, 1)],
            key: Some(OutPoint::new(txid, 2)),
            key_path: Some("00".into()),
            created_at: 1,
            confirmations: 3,
        };
        save(
            &dir,
            &[record(ChainId::Bitcoin), record(ChainId::BitcoinBlake2b)],
        )
        .unwrap();
        assert_eq!(load(&dir, ChainId::Bitcoin), vec![record(ChainId::Bitcoin)]);
        std::fs::write(path(&dir), b"{").unwrap();
        assert!(load(&dir, ChainId::Bitcoin).is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
