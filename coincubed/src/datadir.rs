use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// File inside the datadir that publishes the live RPC socket path.
///
/// The socket itself stays in the temp directory so the path fits `sun_path`.
/// Its name is random and written here at startup, so a client does not have
/// to guess a hash of the datadir and another user cannot squat a predictable
/// name in `/tmp`.
pub const RPC_SOCKET_RECORD: &str = "rpc.socket";

pub struct DataDirectory(PathBuf);

/// Exclusive ownership of this data directory for one daemon process.
///
/// The socket name is random, so binding it does not collide when a second
/// `coincubed` is pointed at the same directory. The lock is what refuses
/// that second process. Drop releases it.
#[derive(Debug)]
pub struct DaemonLock {
    _file: Option<std::fs::File>,
}

impl DataDirectory {
    pub fn new(p: PathBuf) -> Self {
        DataDirectory(p)
    }
}

impl DataDirectory {
    pub fn exists(&self) -> bool {
        self.0.as_path().exists()
    }
    pub fn init(&self) -> Result<(), std::io::Error> {
        #[cfg(unix)]
        return {
            use std::fs::DirBuilder;
            use std::os::unix::fs::DirBuilderExt;

            let mut builder = DirBuilder::new();
            builder.mode(0o700).recursive(true).create(self.path())
        };

        // TODO: permissions on Windows..
        #[cfg(not(unix))]
        return std::fs::create_dir_all(self.path());
    }
    pub fn path(&self) -> &Path {
        self.0.as_path()
    }
    pub fn sqlite_db_file_path(&self) -> PathBuf {
        let mut dir = self.0.clone();
        dir.push("coincubed.sqlite3");
        dir
    }
    pub fn coincubed_watchonly_wallet_path(&self) -> PathBuf {
        let mut dir = self.0.clone();
        dir.push("coincubed_watchonly_wallet");
        dir
    }

    /// Socket path a client should dial.
    ///
    /// A published path is used only while something is listening on it. A
    /// leftover `rpc.socket` from a daemon that exited or crashed is ignored,
    /// and the legacy hashed name is used instead, so this client can still
    /// reach a daemon that bound that older path.
    pub fn coincubed_rpc_socket_path(&self) -> PathBuf {
        if let Some(path) = self.published_rpc_socket_path() {
            if unix_socket_is_listening(&path) {
                return path;
            }
        }
        self.legacy_hashed_rpc_socket_path()
    }

    /// Take the datadir lock, or fail if another daemon already holds it.
    pub fn try_lock(&self) -> io::Result<DaemonLock> {
        std::fs::create_dir_all(self.path())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            use std::os::unix::io::AsRawFd;
            let path = self.path().join("daemon.lock");
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path)?;
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::WouldBlock
                    || err.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || err.raw_os_error() == Some(libc::EAGAIN)
                {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        "another coincubed is already using this data directory",
                    ));
                }
                return Err(err);
            }
            Ok(DaemonLock { _file: Some(file) })
        }
        #[cfg(not(unix))]
        Ok(DaemonLock { _file: None })
    }

    /// Drop the published record when it still names `socket_path`, and remove
    /// the socket file. A newer daemon that already replaced the record is
    /// left alone.
    pub fn retire_published_socket(&self, socket_path: &Path) {
        if self.published_rpc_socket_path().as_deref() == Some(socket_path) {
            let _ = std::fs::remove_file(self.path().join(RPC_SOCKET_RECORD));
        }
        let _ = std::fs::remove_file(socket_path);
    }

    /// Bind address for a newly started RPC server, recorded for clients.
    pub fn publish_rpc_socket(&self) -> io::Result<PathBuf> {
        let socket_name = format!("cc-{}.sock", random_hex(16));
        let path = short_socket_path(&socket_name);
        if let Some(parent) = self.path().parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::create_dir_all(self.path())?;
        let record = self.path().join(RPC_SOCKET_RECORD);
        let tmp = self.path().join(format!("{RPC_SOCKET_RECORD}.tmp"));
        {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(path.to_string_lossy().as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
            }
        }
        std::fs::rename(&tmp, &record)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(path)
    }

    fn published_rpc_socket_path(&self) -> Option<PathBuf> {
        let text = std::fs::read_to_string(self.path().join(RPC_SOCKET_RECORD)).ok()?;
        let path = PathBuf::from(text.trim());
        if path.as_os_str().is_empty() {
            None
        } else {
            Some(path)
        }
    }

    fn legacy_hashed_rpc_socket_path(&self) -> PathBuf {
        let mut hasher = DefaultHasher::new();
        self.0.hash(&mut hasher);
        let hash = hasher.finish();
        short_socket_path(&format!("cc{:016x}.sock", hash))
    }
}

fn short_socket_path(socket_name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(socket_name);
    #[cfg(unix)]
    {
        // macOS uses 104 bytes, Linux usually 108.
        // Use the stricter limit so the path is always safe.
        const MAX_SUN_PATH: usize = 104;
        if path.as_os_str().as_encoded_bytes().len() >= MAX_SUN_PATH {
            return PathBuf::from("/tmp").join(socket_name);
        }
    }
    path
}

fn random_hex(nbytes: usize) -> String {
    let mut bytes = vec![0u8; nbytes];
    #[cfg(unix)]
    {
        if let Ok(mut urandom) = std::fs::File::open("/dev/urandom") {
            if urandom.read_exact(&mut bytes).is_ok() {
                return hex_encode(&bytes);
            }
        }
    }
    // Last resort if the OS RNG cannot be opened. The name is still not the
    // public datadir hash; clients read whatever was published.
    let mixed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = (mixed >> ((i % 8) * 8)) as u8 ^ (i as u8);
    }
    hex_encode(&bytes)
}

fn unix_socket_is_listening(path: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(path).is_ok()
    }
    #[cfg(not(unix))]
    {
        path.exists()
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cc-datadir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_second_lock_is_refused_until_the_first_drops() {
        let dir = scratch();
        let data = DataDirectory::new(dir.clone());
        let first = data.try_lock().unwrap();
        let second = data.try_lock().unwrap_err();
        assert_eq!(second.kind(), io::ErrorKind::AlreadyExists);
        drop(first);
        assert!(data.try_lock().is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dead_socket_record_falls_back_and_a_live_one_is_dialed() {
        let dir = scratch();
        let data = DataDirectory::new(dir.clone());
        data.init().unwrap();
        let missing = dir.join("not-listening.sock");
        std::fs::write(
            dir.join(RPC_SOCKET_RECORD),
            format!("{}\n", missing.display()),
        )
        .unwrap();
        assert_eq!(
            data.coincubed_rpc_socket_path(),
            data.legacy_hashed_rpc_socket_path()
        );

        let sock = std::env::temp_dir().join(format!(
            "cc-live-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        std::fs::write(dir.join(RPC_SOCKET_RECORD), format!("{}\n", sock.display())).unwrap();
        assert_eq!(data.coincubed_rpc_socket_path(), sock);
        drop(listener);
        let _ = std::fs::remove_file(&sock);
        assert_eq!(
            data.coincubed_rpc_socket_path(),
            data.legacy_hashed_rpc_socket_path()
        );

        data.retire_published_socket(&missing);
        assert!(dir.join(RPC_SOCKET_RECORD).exists());
        data.retire_published_socket(&sock);
        assert!(!dir.join(RPC_SOCKET_RECORD).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_running_daemon_holds_the_datadir_lock() {
        let ms = crate::testutils::DummyCoincube::new(
            crate::testutils::DummyBitcoind::new(),
            crate::testutils::DummyDatabase::new(),
        );
        let data = DataDirectory::new(ms.tmp_dir.join("d").join("bitcoin"));
        let err = data.try_lock().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        ms.shutdown();
    }
}
