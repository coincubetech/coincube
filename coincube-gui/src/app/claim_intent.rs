//! "Start a claim as soon as this Cube is open."
//!
//! The Home card that starts a Bitcoin Blake2b claim is pressed *before* the
//! source Cube is unlocked — the claim needs its descriptor and its master
//! seed, and Home has neither. So the card records an intent, the ordinary
//! unlock runs, and the freshly built `App` consumes it.
//!
//! One Cube id, not a queue: a second press replaces the first, and opening
//! any other Cube clears it — *every* open consumes, including a Cube that
//! turns out to have no Vault (`App::new_without_wallet`), because an intent
//! that survived an unrelated open would fire on a later one the user did not
//! ask a claim for. It holds no secret — only which Cube the user
//! pressed the card on — and it is never permission: `App` re-checks
//! [`crate::app::features::claim_blake2b`] before the installer starts, and
//! `Installer::try_new_for_chain` re-checks the account gate after that.

use crate::dir::CoincubeDirectory;
use std::sync::{Mutex, OnceLock};

/// Navigation only. No keys, session credentials, or signing authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkHandoff {
    root: std::path::PathBuf,
    bitcoin_cube: String,
    fork_cube: String,
}
impl ForkHandoff {
    pub fn new(
        root: &CoincubeDirectory,
        bitcoin_cube: String,
        fork_cube: String,
    ) -> Result<Self, String> {
        if bitcoin_cube.is_empty() || fork_cube.is_empty() || bitcoin_cube == fork_cube {
            return Err("The Claim must name two distinct Cubes.".into());
        }
        Ok(Self {
            root: root.path().to_path_buf(),
            bitcoin_cube,
            fork_cube,
        })
    }
    pub fn matches_root(&self, root: &CoincubeDirectory) -> bool {
        self.root == root.path()
    }
    /// Discover a restart route only when exactly one matching source has a
    /// recorded Claim. Ambiguity requires the explicit source-Cube handoff.
    pub fn discover(
        root: &CoincubeDirectory,
        fork_cube: &str,
        wallet: &crate::app::wallet::Wallet,
    ) -> Option<Self> {
        use crate::{app::settings::Settings, chain::ChainId};
        if wallet.chain != ChainId::BitcoinBlake2b {
            return None;
        }
        let bitcoin = root.network_directory(ChainId::Bitcoin);
        if !bitcoin
            .path()
            .join(crate::app::settings::SETTINGS_FILE_NAME)
            .is_file()
        {
            return None;
        }
        let sources = Settings::from_file(&bitcoin).ok()?;
        let mut candidates = sources.cubes.iter().filter_map(|source| {
            let id = source.vault_wallet_id.as_ref()?;
            if source.network != ChainId::Bitcoin
                || id.descriptor_checksum != wallet.descriptor_checksum
            {
                return None;
            }
            let pairing = crate::app::state::vault::claim::pairing::Pairing::read(
                root, &source.id, fork_cube, wallet,
            )
            .ok()?;
            if std::fs::symlink_metadata(pairing.journal_directory(root).join("intent.json"))
                .is_err()
            {
                return None;
            }
            Self::new(root, source.id.clone(), fork_cube.into()).ok()
        });
        let first = candidates.next()?;
        if candidates.next().is_some() {
            None
        } else {
            Some(first)
        }
    }
    /// Fresh navigation lookup only. The unlocked loader verifies the actual
    /// descriptor, authenticated account, provider, and existing journal later.
    pub fn resolve_target(
        &self,
        root: &CoincubeDirectory,
    ) -> Result<crate::app::settings::CubeSettings, String> {
        use crate::{
            app::settings::{CubeSettings, Settings},
            chain::ChainId,
        };
        if !self.matches_root(root) {
            return Err("The Claim belongs to a different data directory.".into());
        }
        let source = Settings::from_file(&root.network_directory(ChainId::Bitcoin))
            .map_err(|e| e.to_string())?;
        let target = Settings::from_file(&root.network_directory(ChainId::BitcoinBlake2b))
            .map_err(|e| e.to_string())?;
        let unique = |cubes: Vec<CubeSettings>,
                      id: &str,
                      chain: ChainId|
         -> Result<CubeSettings, String> {
            let mut matches = cubes.into_iter().filter(|c| c.id == id);
            let cube = matches
                .next()
                .ok_or_else(|| "A paired Claim Cube is missing.".to_string())?;
            if matches.next().is_some() || cube.network != chain {
                return Err("A paired Claim Cube has an ambiguous or wrong-chain identity.".into());
            }
            Ok(cube)
        };
        let source = unique(source.cubes, &self.bitcoin_cube, ChainId::Bitcoin)?;
        let target = unique(target.cubes, &self.fork_cube, ChainId::BitcoinBlake2b)?;
        let same_descriptor = source
            .vault_wallet_id
            .as_ref()
            .zip(target.vault_wallet_id.as_ref())
            .is_some_and(|(s, t)| s.descriptor_checksum == t.descriptor_checksum);
        let same_fingerprint = source
            .vault_fingerprint
            .as_ref()
            .zip(target.vault_fingerprint.as_ref())
            .is_some_and(|(s, t)| !s.is_empty() && s == t);
        if !same_descriptor || !same_fingerprint {
            return Err(
                "The paired Claim Vaults changed. Return to the Bitcoin Cube to check them.".into(),
            );
        }
        Ok(target)
    }
    pub fn bitcoin_cube(&self) -> &str {
        &self.bitcoin_cube
    }
    pub fn fork_cube(&self) -> &str {
        &self.fork_cube
    }
}
#[derive(Debug)]
pub enum Intent {
    Bitcoin(String),
    Fork(ForkHandoff),
}
fn cell() -> &'static Mutex<Option<Intent>> {
    static INTENT: OnceLock<Mutex<Option<Intent>>> = OnceLock::new();
    INTENT.get_or_init(|| Mutex::new(None))
}

pub fn arm_fork(handoff: ForkHandoff) {
    if let Ok(mut slot) = cell().lock() {
        *slot = Some(Intent::Fork(handoff));
    }
}
/// Every open consumes the intent, including wrong-root and wrong-Cube opens.
pub fn take_for_cube(root: &CoincubeDirectory, cube_id: &str) -> Option<Intent> {
    let intent = cell().lock().ok()?.take()?;
    match &intent {
        Intent::Bitcoin(id) if id == cube_id => Some(intent),
        Intent::Fork(pair) if pair.matches_root(root) && pair.fork_cube == cube_id => Some(intent),
        _ => None,
    }
}

/// Record that the next open of `cube_id` should start a claim.
pub fn arm(cube_id: impl Into<String>) {
    if let Ok(mut slot) = cell().lock() {
        *slot = Some(Intent::Bitcoin(cube_id.into()));
    }
}

/// Consume the intent if it was armed for this Cube.
///
/// Takes on *any* Cube open, matching or not: an intent that survived the user
/// changing their mind and opening something else would fire on a later,
/// unrelated open.
pub fn take(cube_id: &str) -> bool {
    match cell().lock() {
        Ok(mut slot) => slot
            .take()
            .is_some_and(|armed| matches!(armed, Intent::Bitcoin(id) if id == cube_id)),
        Err(_) => false,
    }
}

/// Drop any armed intent. Called on lock, on duress, and on the per-Cube
/// revocation path alongside the session it was armed with.
pub fn clear() {
    if let Ok(mut slot) = cell().lock() {
        *slot = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One global cell, so the cases run in one test rather than racing.
    #[test]
    fn an_intent_fires_once_for_its_own_cube_and_never_for_another() {
        let _guard = crate::app::session::test_guard();
        clear();
        assert!(!take("cube-a"), "nothing armed");

        arm("cube-a");
        assert!(take("cube-a"), "armed for this Cube");
        assert!(!take("cube-a"), "and only once");

        arm("cube-a");
        assert!(!take("cube-b"), "opening another Cube consumes it");
        assert!(!take("cube-a"), "without firing later");

        arm("cube-a");
        arm("cube-b");
        assert!(!take("cube-a"), "a second press replaces the first");
        let root = CoincubeDirectory::new("/synthetic-claim-intent".into());
        let other = CoincubeDirectory::new("/other-claim-intent".into());
        let pair = ForkHandoff::new(&root, "source".into(), "target".into()).unwrap();
        arm_fork(pair.clone());
        assert!(matches!(
            take_for_cube(&root, "target"),
            Some(Intent::Fork(_))
        ));
        assert!(take_for_cube(&root, "target").is_none());
        arm_fork(pair.clone());
        assert!(take_for_cube(&other, "target").is_none());
        assert!(take_for_cube(&root, "target").is_none());
        arm_fork(pair);
        assert!(take_for_cube(&root, "source").is_none());
        assert!(take_for_cube(&root, "target").is_none());
        clear();
    }
}
