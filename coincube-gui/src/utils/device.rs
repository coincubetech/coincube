use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

#[cfg(not(test))]
fn device_fingerprint_path() -> Option<PathBuf> {
    dirs::config_dir().map(|base| base.join("coincube").join("device_fingerprint"))
}

/// Every test that builds a `CoincubeClient` reaches this through
/// `device_headers()`. It must not read or create the developer's real
/// fingerprint (on macOS, the live Coincube datadir's `device_fingerprint`).
#[cfg(test)]
fn device_fingerprint_path() -> Option<PathBuf> {
    Some(
        std::env::temp_dir()
            .join(format!("coincube-test-device-{}", std::process::id()))
            .join("device_fingerprint"),
    )
}

pub fn get_or_create_device_fingerprint() -> String {
    let Some(path) = device_fingerprint_path() else {
        return Uuid::new_v4().to_string();
    };
    if let Ok(existing) = fs::read_to_string(&path) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let fingerprint = Uuid::new_v4().to_string();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&path, &fingerprint);
    fingerprint
}

/// The name this computer registers under as a Connect signer device for a
/// Cube on `chain`.
///
/// A Bitcoin Blake2b Cube registers its own signer device, with the transport
/// key of its own chain folder, so one computer has a second device row on the
/// server. The chain is appended there so the two can be told apart. On every
/// other chain this is [`device_label`], unchanged: the first registration
/// names a device for good, so the name must not move for existing installs.
pub fn signer_device_label(chain: crate::chain::ChainId) -> String {
    let label = device_label();
    match chain {
        crate::chain::ChainId::BitcoinBlake2b => format!("{label} · Bitcoin Blake2b"),
        crate::chain::ChainId::BitcoinBlake2bTestnet4 => {
            format!("{label} · Bitcoin Blake2b testnet4")
        }
        _ => label,
    }
}

pub fn device_label() -> String {
    let host = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "Unknown Host".to_string());
    let os = std::env::consts::OS; // "macos", "windows", "linux"
    let os_label = match os {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    };
    format!("{host} ({os_label})")
}

pub fn device_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "X-Device-Fingerprint",
        reqwest::header::HeaderValue::from_str(&get_or_create_device_fingerprint())
            .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("unknown")),
    );
    headers.insert(
        "X-Device-Name",
        reqwest::header::HeaderValue::from_str(&device_label())
            .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("unknown")),
    );
    headers.insert(
        "X-Platform",
        reqwest::header::HeaderValue::from_static("desktop"),
    );
    headers
}

#[cfg(test)]
mod tests {
    #[test]
    fn fingerprint_path_is_never_the_os_config_dir_in_tests() {
        let path = super::device_fingerprint_path().unwrap();
        assert!(path.starts_with(std::env::temp_dir()), "{:?}", path);
        if let Some(config) = dirs::config_dir() {
            assert!(!path.starts_with(config), "{:?}", path);
        }

        // Other tests may race to create the file, so check where it landed,
        // not which test's value won.
        assert!(!super::get_or_create_device_fingerprint().is_empty());
        assert!(path.is_file(), "{:?}", path);
    }

    /// The first registration names a signer device for good, so the name a
    /// Bitcoin-family Cube registers under must not move. Only a Bitcoin
    /// Blake2b Cube, which registers a device of its own, says its chain.
    #[test]
    fn signer_device_label_names_the_chain_only_on_bitcoin_blake2b() {
        use crate::chain::ChainId;

        let plain = super::device_label();
        for chain in ChainId::ALL {
            let label = super::signer_device_label(chain);
            if chain.is_blake2b() {
                assert!(label.starts_with(&format!("{plain} · ")), "{}", label);
                assert!(label.contains("Bitcoin Blake2b"), "{}", label);
            } else {
                assert_eq!(label, plain, "{chain:?}");
            }
        }
        assert_ne!(
            super::signer_device_label(ChainId::BitcoinBlake2b),
            super::signer_device_label(ChainId::BitcoinBlake2bTestnet4),
        );
    }
}
