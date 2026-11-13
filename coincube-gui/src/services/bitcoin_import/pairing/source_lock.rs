//! A per-source lock for imported-wallet receive allocation. Address
//! allocation for one public source (`SplitSource` digest) is serialized
//! through a private, nonblocking lock file under the Bitcoin Blake2b network
//! directory, so two Cube sessions never hand out the same index.
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
};

use coincube_core::public_wallet_source::SplitSource;
use fs4::fs_std::FileExt;

use crate::{chain::ChainId, dir::CoincubeDirectory};

const BUSY_OR_UNSAFE: &str = "Another session is reserving addresses for this imported wallet, or its lock cannot be safely checked. Retry after it finishes.";

/// The same nonblocking guard also protects in-app imported receive allocation.
/// Hold it before taking a wallet database or journal lock, never the reverse.
pub(crate) fn source_reservation_lock(
    root: &CoincubeDirectory,
    source: &SplitSource,
) -> Result<File, String> {
    let directory = root
        .network_directory(ChainId::BitcoinBlake2b)
        .path()
        .join("split-source-locks")
        .join(source.digest().to_string());
    prepare_directory(&directory).map_err(|_| BUSY_OR_UNSAFE.to_string())?;
    source_lock(&directory.join("reservation.lock"))
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
    let file = options.open(path).map_err(|_| BUSY_OR_UNSAFE.to_string())?;
    let metadata = file.metadata().map_err(|_| BUSY_OR_UNSAFE.to_string())?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(BUSY_OR_UNSAFE.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(BUSY_OR_UNSAFE.into());
        }
    }
    if !file
        .try_lock_exclusive()
        .map_err(|_| BUSY_OR_UNSAFE.to_string())?
    {
        return Err(BUSY_OR_UNSAFE.into());
    }
    Ok(file)
}

/// Create the lock directory privately: new components are owner-only and an
/// existing directory must already be private and owned by this user.
fn prepare_directory(directory: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        match fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
        {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(std::io::Error::other("unsafe lock directory"));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(directory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(seed: u8) -> SplitSource {
        use coincube_core::miniscript::{
            bitcoin::{
                bip32::{Xpriv, Xpub},
                secp256k1::Secp256k1,
                Network,
            },
            Descriptor,
        };
        use std::str::FromStr;
        let secp = Secp256k1::new();
        let master = Xpriv::new_master(Network::Bitcoin, &[seed; 32]).unwrap();
        let key = Xpub::from_priv(&secp, &master);
        SplitSource::new(
            Descriptor::from_str(&format!("wpkh({key}/0/*)")).unwrap(),
            Some(Descriptor::from_str(&format!("wpkh({key}/1/*)")).unwrap()),
        )
        .unwrap()
    }

    #[test]
    fn source_lock_serializes_competing_sessions_and_releases_on_drop() {
        let root = CoincubeDirectory::new(
            std::env::temp_dir().join(format!("imported-source-lock-{}", uuid::Uuid::new_v4())),
        );
        let held = source_reservation_lock(&root, &source(47)).unwrap();
        assert!(source_reservation_lock(&root, &source(47)).is_err());
        assert!(source_reservation_lock(&root, &source(48)).is_ok());
        drop(held);
        assert!(source_reservation_lock(&root, &source(47)).is_ok());
        fs::remove_dir_all(root.path()).unwrap();
    }
}
