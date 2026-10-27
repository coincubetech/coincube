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
    /// Socket path a client should dial. Prefers the path the running daemon
    /// published. Falls back to the legacy hashed name so a client can still
    /// find a daemon that started before this record existed.
    pub fn coincubed_rpc_socket_path(&self) -> PathBuf {
        self.published_rpc_socket_path()
            .unwrap_or_else(|| self.legacy_hashed_rpc_socket_path())
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

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0xf) as usize] as char);
    }
    out
}
