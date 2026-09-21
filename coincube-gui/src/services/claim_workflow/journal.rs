use super::{Error, Intent};
use fs4::fs_std::FileExt;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
const MAX_BYTES: u64 = 1024 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(0);

/// Stable lock inode is never renamed or unlinked. All cooperating owners hold
/// this handle for the complete controller lifetime, including observation I/O.
pub(super) struct Journal {
    directory: PathBuf,
    _lock: File,
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
    #[cfg(not(unix))]
    {
        let _ = (path, create);
        Err(Error::UnsupportedPlatform)
    }
}
impl Journal {
    pub(super) fn open(directory: &Path) -> Result<Self, Error> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::symlink_metadata(directory)?;
            if !metadata.is_dir()
                || metadata.mode() & 0o077 != 0
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err(Error::InvalidJournal);
            }
        }
        #[cfg(not(unix))]
        {
            return Err(Error::UnsupportedPlatform);
        }
        let directory = fs::canonicalize(directory)?;
        let lock = private_file(&directory.join("claim.lock"), true)?;
        if !lock.try_lock_exclusive()? {
            return Err(Error::Busy);
        }
        let mut journal = Self {
            directory,
            _lock: lock,
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
        if file.metadata()?.len() > MAX_BYTES {
            return Err(Error::InvalidJournal);
        }
        let mut bytes = Vec::new();
        (&mut file).take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Error::InvalidJournal);
        }
        Ok(Some(bytes))
    }
    pub(super) fn load(&self) -> Result<Option<Intent>, Error> {
        self.snapshot
            .as_ref()
            .map(|bytes| serde_json::from_slice(bytes).map_err(|_| Error::InvalidJournal))
            .transpose()
    }
    pub(super) fn store(&mut self, intent: &Intent) -> Result<(), Error> {
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
        let bytes = serde_json::to_vec(intent).map_err(|_| Error::InvalidJournal)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(Error::InvalidJournal);
        }
        let path = self.directory.join(format!(
            ".intent-{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut created = false;
        let result = (|| -> Result<(), Error> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
            }
            let mut file = options.open(&path)?;
            created = true;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&path, self.directory.join("intent.json"))?;
            File::open(&self.directory)?.sync_all()?;
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
}
