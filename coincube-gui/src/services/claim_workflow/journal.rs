#[cfg(not(windows))]
use std::fs::OpenOptions;
#[cfg(windows)]
mod windows;

use super::{Error, Intent};
use fs4::fs_std::FileExt;
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
const LEGACY_MAX_BYTES: u64 = 1024 * 1024;
// One bounded hex ancestry record, atomically replaced with its intent. No
// second file can get out of sync with the selected input or transaction.
const MAX_BYTES: u64 =
    LEGACY_MAX_BYTES + 2 * coincube_core::claim_ancestry::retained::MAX_ENCODED_BYTES as u64 + 1024;
const ANCESTRY_PAIR_MAX_BYTES: u64 = LEGACY_MAX_BYTES
    + 4 * coincube_core::claim_ancestry::retained::MAX_ENCODED_BYTES as u64
    + 1024 * 1024;
fn intent_limit(intent: &Intent) -> u64 {
    if (intent.version == 10 && intent.funding_plan.is_some()) || intent.version == 11 {
        ANCESTRY_PAIR_MAX_BYTES
    } else if intent.version == 7 && intent.ancestry.is_some() {
        MAX_BYTES
    } else {
        LEGACY_MAX_BYTES
    }
}
static NEXT: AtomicU64 = AtomicU64::new(0);
const SPLIT_FUNDING_FILE: &str = "split-funding.json";
const SPLIT_FUNDING_MAX_BYTES: u64 = 64 * 1024;
const CLAIM_FUNDING_FILE: &str = "claim-funding.json";
const CLAIM_FUNDING_MAX_BYTES: u64 = 64 * 1024;

/// Stable lock inode is never renamed or unlinked. All cooperating owners hold
/// this handle for the complete controller lifetime, including observation I/O.
pub(super) struct Journal {
    directory: PathBuf,
    _lock: File,
    #[cfg(windows)]
    _directory: windows::Directory,
    snapshot: Option<Vec<u8>>,
    poisoned: bool,
}
fn private_file(path: &Path, create: bool) -> Result<File, Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(create)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(Error::InvalidJournal);
        }
        Ok(file)
    }
    #[cfg(windows)]
    {
        windows::file(path, create).map_err(Error::Io)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, create);
        Err(Error::UnsupportedPlatform)
    }
}
/// Create the private journal directory without following a final-component
/// symlink or changing permissions on an existing object. Platform backends
/// must establish privacy at creation, before any intent can be written.
pub(super) fn prepare_directory(directory: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        // A freshly installed wallet may not have materialized all parents yet.
        // New components are private; existing components are never chmodded.
        match fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
        {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        validate_directory(directory)
    }
    #[cfg(windows)]
    {
        windows::Directory::open(directory, true)
            .map(|_| ())
            .map_err(Error::Io)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = directory;
        Err(Error::UnsupportedPlatform)
    }
}

#[cfg(unix)]
fn validate_directory(directory: &Path) -> Result<(), Error> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir()
        || metadata.mode() & 0o077 != 0
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(Error::InvalidJournal);
    }
    Ok(())
}

impl Journal {
    pub(super) fn open(directory: &Path) -> Result<Self, Error> {
        #[cfg(unix)]
        validate_directory(directory)?;
        #[cfg(not(any(unix, windows)))]
        {
            return Err(Error::UnsupportedPlatform);
        }
        #[cfg(windows)]
        let pinned_directory = windows::Directory::open(directory, false)?;
        #[cfg(windows)]
        let directory = pinned_directory.path.clone();
        #[cfg(not(windows))]
        let directory = fs::canonicalize(directory)?;
        let lock = private_file(&directory.join("claim.lock"), true)?;
        if !lock.try_lock_exclusive()? {
            return Err(Error::Busy);
        }
        let mut journal = Self {
            directory,
            _lock: lock,
            #[cfg(windows)]
            _directory: pinned_directory,
            snapshot: None,
            poisoned: false,
        };
        journal.snapshot = journal.read()?;
        Ok(journal)
    }
    fn read(&self) -> Result<Option<Vec<u8>>, Error> {
        let mut file = match private_file(&self.directory.join("intent.json"), false) {
            Ok(file) => file,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > ANCESTRY_PAIR_MAX_BYTES {
            return Err(Error::InvalidJournal);
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(ANCESTRY_PAIR_MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > ANCESTRY_PAIR_MAX_BYTES {
            return Err(Error::InvalidJournal);
        }
        Ok(Some(bytes))
    }
    pub(super) fn load(&self) -> Result<Option<Intent>, Error> {
        self.snapshot
            .as_ref()
            .map(|bytes| {
                let intent: Intent =
                    serde_json::from_slice(bytes).map_err(|_| Error::InvalidJournal)?;
                if bytes.len() as u64 > intent_limit(&intent) {
                    return Err(Error::InvalidJournal);
                }
                Ok(intent)
            })
            .transpose()
    }
    pub(super) fn load_split_funding_reservation(
        &self,
    ) -> Result<Option<super::split_funding::SplitFundingReservation>, Error> {
        let mut file = match private_file(&self.directory.join(SPLIT_FUNDING_FILE), false) {
            Ok(file) => file,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > SPLIT_FUNDING_MAX_BYTES {
            return Err(Error::InvalidJournal);
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(SPLIT_FUNDING_MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > SPLIT_FUNDING_MAX_BYTES {
            return Err(Error::InvalidJournal);
        }
        let saved: super::split_funding::SplitFundingReservation =
            serde_json::from_slice(&bytes).map_err(|_| Error::InvalidJournal)?;
        saved.validate().map_err(|_| Error::InvalidJournal)?;
        Ok(Some(saved))
    }
    pub(super) fn load_claim_funding_reservation(
        &self,
    ) -> Result<Option<super::claim_funding::ClaimFundingReservation>, Error> {
        let mut file = match private_file(&self.directory.join(CLAIM_FUNDING_FILE), false) {
            Ok(file) => file,
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > CLAIM_FUNDING_MAX_BYTES {
            return Err(Error::InvalidJournal);
        }
        let mut bytes = Vec::new();
        (&mut file)
            .take(CLAIM_FUNDING_MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > CLAIM_FUNDING_MAX_BYTES {
            return Err(Error::InvalidJournal);
        }
        let saved: super::claim_funding::ClaimFundingReservation =
            serde_json::from_slice(&bytes).map_err(|_| Error::InvalidJournal)?;
        saved.validate().map_err(|_| Error::InvalidJournal)?;
        Ok(Some(saved))
    }

    pub(super) fn store_claim_funding_reservation(
        &mut self,
        reservation: &super::claim_funding::ClaimFundingReservation,
    ) -> Result<(), Error> {
        self.ensure_current()?;
        if self.load_claim_funding_reservation()?.is_some() {
            return Err(Error::Conflict);
        }
        self.write_claim_funding_reservation(reservation)
    }

    pub(super) fn release_claim_funding_reservation(
        &mut self,
        expected: &super::claim_funding::ClaimFundingReservation,
    ) -> Result<super::claim_funding::ClaimFundingReservation, Error> {
        self.ensure_current()?;
        let saved = self
            .load_claim_funding_reservation()?
            .ok_or(Error::InvalidJournal)?;
        if saved != *expected {
            return Err(Error::Conflict);
        }
        if saved.is_released() {
            return Ok(saved);
        }
        let released = saved.released_copy();
        self.write_claim_funding_reservation(&released)?;
        Ok(released)
    }

    pub(super) fn record_claim_change_indices(
        &mut self,
        expected: &super::claim_funding::ClaimFundingReservation,
        change: super::claim_funding::ClaimChangeIndices,
    ) -> Result<super::claim_funding::ClaimFundingReservation, Error> {
        self.ensure_current()?;
        let saved = self
            .load_claim_funding_reservation()?
            .ok_or(Error::InvalidJournal)?;
        if saved != *expected
            || self.load()?.is_some()
            || self.load_split_funding_reservation()?.is_some()
            || std::fs::symlink_metadata(self.directory.join(super::SPLIT_TOMBSTONE)).is_ok()
        {
            return Err(Error::Conflict);
        }
        if saved.change_indices() == Some(change) {
            return Ok(saved);
        }
        let recorded = saved.with_change_indices(change)?;
        self.write_claim_funding_reservation(&recorded)?;
        Ok(recorded)
    }

    fn write_claim_funding_reservation(
        &mut self,
        reservation: &super::claim_funding::ClaimFundingReservation,
    ) -> Result<(), Error> {
        let bytes = serde_json::to_vec(reservation).map_err(|_| Error::InvalidPlan)?;
        if bytes.len() as u64 > CLAIM_FUNDING_MAX_BYTES {
            return Err(Error::InvalidPlan);
        }
        let path = self.directory.join(format!(
            ".claim-funding-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> Result<(), Error> {
            #[cfg(not(windows))]
            let mut file = {
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
                }
                options.open(&path)?
            };
            #[cfg(windows)]
            let mut file = windows::temporary(&path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            #[cfg(windows)]
            windows::replace(&path, &self.directory.join(CLAIM_FUNDING_FILE))?;
            #[cfg(not(windows))]
            {
                fs::rename(&path, self.directory.join(CLAIM_FUNDING_FILE))?;
                File::open(&self.directory)?.sync_all()?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
            let _ = fs::remove_file(path);
        }
        result
    }
    pub(super) fn store_split_funding_reservation(
        &mut self,
        reservation: &super::split_funding::SplitFundingReservation,
    ) -> Result<(), Error> {
        self.ensure_current()?;
        if self.load_split_funding_reservation()?.is_some() {
            return Err(Error::Conflict);
        }
        self.write_split_funding_reservation(reservation)
    }

    pub(super) fn release_split_funding_reservation(
        &mut self,
        expected: &super::split_funding::SplitFundingReservation,
    ) -> Result<super::split_funding::SplitFundingReservation, Error> {
        self.ensure_current()?;
        let saved = self
            .load_split_funding_reservation()?
            .ok_or(Error::InvalidJournal)?;
        if saved != *expected {
            return Err(Error::Conflict);
        }
        if saved.is_released() {
            return Ok(saved);
        }
        let released = saved.released_copy();
        self.write_split_funding_reservation(&released)?;
        Ok(released)
    }

    fn write_split_funding_reservation(
        &mut self,
        reservation: &super::split_funding::SplitFundingReservation,
    ) -> Result<(), Error> {
        let bytes = serde_json::to_vec(reservation).map_err(|_| Error::InvalidPlan)?;
        if bytes.len() as u64 > SPLIT_FUNDING_MAX_BYTES {
            return Err(Error::InvalidPlan);
        }
        let path = self.directory.join(format!(
            ".split-funding-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> Result<(), Error> {
            #[cfg(not(windows))]
            let mut file = {
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
                }
                options.open(&path)?
            };
            #[cfg(windows)]
            let mut file = windows::temporary(&path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            #[cfg(windows)]
            windows::replace(&path, &self.directory.join(SPLIT_FUNDING_FILE))?;
            #[cfg(not(windows))]
            {
                fs::rename(&path, self.directory.join(SPLIT_FUNDING_FILE))?;
                File::open(&self.directory)?.sync_all()?;
            }
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
            let _ = fs::remove_file(path);
        }
        result
    }
    pub(super) fn ensure_current(&mut self) -> Result<(), Error> {
        if self.poisoned {
            return Err(Error::InvalidJournal);
        }
        // A noncooperating writer is not silently overwritten.
        let disk = match self.read() {
            Ok(disk) => disk,
            Err(error) => {
                self.poisoned = true;
                return Err(error);
            }
        };
        if disk != self.snapshot {
            self.poisoned = true;
            return Err(Error::Conflict);
        }
        Ok(())
    }
    pub(super) fn store(&mut self, intent: &Intent) -> Result<(), Error> {
        self.ensure_current()?;
        let bytes = serde_json::to_vec(intent).map_err(|_| Error::InvalidJournal)?;
        if bytes.len() as u64 > intent_limit(intent) {
            return Err(Error::InvalidJournal);
        }
        let path = self.directory.join(format!(
            ".intent-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut created = false;
        let result = (|| -> Result<(), Error> {
            #[cfg(not(windows))]
            let mut file = {
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
                }
                options.open(&path)?
            };
            #[cfg(windows)]
            let mut file = windows::temporary(&path)?;
            created = true;
            #[cfg(test)]
            crash_boundary("created", &self.directory, intent);
            file.write_all(&bytes)?;
            file.sync_all()?;
            #[cfg(test)]
            crash_boundary("flushed", &self.directory, intent);
            drop(file);
            #[cfg(windows)]
            windows::replace(&path, &self.directory.join("intent.json"))?;
            #[cfg(not(windows))]
            {
                fs::rename(&path, self.directory.join("intent.json"))?;
                File::open(&self.directory)?.sync_all()?;
            }
            #[cfg(test)]
            crash_boundary("replaced", &self.directory, intent);
            Ok(())
        })();
        if result.is_err() {
            // After rename a failed directory sync has an uncertain durability
            // outcome. Never keep operating from the old in-memory snapshot.
            self.poisoned = true;
            if created {
                let _ = fs::remove_file(path);
            }
            return result;
        }
        self.snapshot = Some(bytes);
        Ok(())
    }
    /// Delete the intent file, still holding the lock. Refuses a file some
    /// other writer changed. Afterwards this owner holds no intent and cannot
    /// store one; a later open finds none. The lock file stays, as always.
    pub(super) fn remove(&mut self) -> Result<(), Error> {
        self.ensure_current()?;
        let result = (|| -> Result<(), Error> {
            fs::remove_file(self.directory.join("intent.json"))?;
            #[cfg(not(windows))]
            File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        // Whatever the outcome, this owner's snapshot no longer describes the
        // disk with certainty; it must not write again.
        self.poisoned = true;
        self.snapshot = None;
        result
    }
}

// Only the explicitly selected subprocess fixture can pause here. Production
// builds contain neither the environment hooks nor the pause.
#[cfg(test)]
fn crash_boundary(stage: &str, directory: &Path, intent: &Intent) {
    if intent.phase != super::Phase::BroadcastUncertain
        || std::env::var("COINCUBE_TEST_CLAIM_CRASH_STAGE")
            .ok()
            .as_deref()
            != Some(stage)
        || std::env::var_os("COINCUBE_TEST_CLAIM_CRASH_DIRECTORY")
            .is_none_or(|path| Path::new(&path) != directory)
    {
        return;
    }
    use std::io::Write;
    println!("CLAIM_WRITE_BOUNDARY_REACHED");
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::park();
    }
}
