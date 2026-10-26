//! Escalating delay on wrong PIN attempts.
//!
//! # This is hygiene, not a security control
//!
//! An offline attacker with the datadir never touches the UI — they run
//! Argon2id in a loop against the seed file and this code is not in their way.
//! Say so plainly rather than letting anyone treat it as a defence.
//!
//! What it *does* buy: a laptop thief who opens the app and starts typing
//! doesn't get 10,000 free guesses through the front door. At 831 ms a guess
//! they were already looking at ~2.3 hours; the escalating delay turns that
//! into "not by hand".
//!
//! # Why the state does not live in settings.json
//!
//! Because the user can trivially delete settings.json, and so can the thief.
//! The counter lives in its own file **outside** any network directory —
//! alongside the duress local state, which is kept there for the same reason.
//! Deleting it is still possible; it is just no longer a side effect of the
//! obvious "reset my settings" move.
//!
//! It is deliberately **not** inside a Cube's data directory either: a duress
//! wipe takes those, and a wipe must not hand the attacker a fresh counter.

use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use coincube_core::signer::SignerError;

use super::{CubeLocation, PinOutcome, UnlockError};

const FILE_NAME: &str = "unlock-throttle.json";
const LOCK_NAME: &str = "unlock-throttle.lock";

/// One serialized PIN trial, shared by normal Cube open and a temporary
/// destination unlock. The lock covers the persisted check, seed/duress trial,
/// and outcome write, including attempts made by another app process.
pub enum PinAttempt {
    Locked(Duration),
    Classified(PinOutcome),
}

/// A permanent-seed export trial. `Words` is zeroizing from the moment it
/// leaves the locked check; no duress outcome can carry a seed or seed blob.
pub enum SeedExportAttempt {
    Words(Zeroizing<Vec<String>>),
    Duress,
    Wrong(Duration),
    Locked(Duration),
}

/// One-use evidence that this root/Cube's marker opened under the submitted
/// PIN. Only `classify_seed_export_cube` can mint it. UI navigation may revoke
/// a pending seed reveal, but cannot revoke a confirmed emergency activation.
#[derive(Clone)]
pub struct VerifiedSeedExportDuress {
    root: PathBuf,
    cube_id: String,
    chain: crate::chain::ChainId,
    app: crate::app::cache::AppGeneration,
    account_id: Option<String>,
    claimed: Arc<AtomicBool>,
}

impl std::fmt::Debug for VerifiedSeedExportDuress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VerifiedSeedExportDuress(<redacted>)")
    }
}

impl VerifiedSeedExportDuress {
    pub(crate) fn origin(
        &self,
    ) -> (
        &Path,
        &str,
        crate::chain::ChainId,
        crate::app::cache::AppGeneration,
    ) {
        (&self.root, &self.cube_id, self.chain, self.app)
    }
    /// Consume the capability once. Root and account came from the verified
    /// trial, not whichever Cube happens to be visible when it resolves.
    pub(crate) fn claim(
        self,
    ) -> Option<(
        PathBuf,
        String,
        crate::chain::ChainId,
        crate::app::cache::AppGeneration,
        Option<String>,
    )> {
        (!self.claimed.swap(true, Ordering::AcqRel)).then_some((
            self.root,
            self.cube_id,
            self.chain,
            self.app,
            self.account_id,
        ))
    }
}

#[cfg(test)]
pub(crate) fn verified_seed_export_duress_for_test(
    root: PathBuf,
    cube_id: String,
    chain: crate::chain::ChainId,
    app: crate::app::cache::AppGeneration,
) -> VerifiedSeedExportDuress {
    VerifiedSeedExportDuress {
        root,
        cube_id,
        chain,
        app,
        account_id: None,
        claimed: Arc::new(AtomicBool::new(false)),
    }
}

pub enum SeedExportCubeAttempt {
    Words(Zeroizing<Vec<String>>),
    Duress(VerifiedSeedExportDuress),
    Wrong(Duration),
    Locked(Duration),
}

/// Share the ordinary Cube PIN's serialized throttle and duress policy across
/// settings surfaces that can reveal the permanent master seed. The first
/// verifier preserves the active session's correct-PIN fast path; on an
/// incorrect PIN, `unlock_blocking` applies the existing seed/marker/keystore
/// classification at full cost. Wrong and duress both take that path.
///
/// `validate` must re-read the exact Cube/chain/marker selection. It runs under
/// the attempt lock before decryption and before a successful or wrong result
/// is committed. A verified duress marker remains an emergency signal even if
/// the selection changes while Argon2 is running, as in `classify_pin_scoped`.
pub fn classify_seed_export(
    root: &Path,
    loc: &CubeLocation,
    pin: &str,
    verify: impl FnOnce() -> Result<Vec<String>, SignerError>,
    validate: impl Fn() -> Result<(), UnlockError>,
) -> Result<SeedExportAttempt, String> {
    with_locked_throttle(root, |state| {
        let remaining = state.remaining_lockout(loc.cube_id);
        if !remaining.is_zero() {
            return Ok(SeedExportAttempt::Locked(remaining));
        }
        validate().map_err(|error| error.to_string())?;
        match verify() {
            Ok(words) => {
                let words = Zeroizing::new(words);
                validate().map_err(|error| error.to_string())?;
                state.record_success(root, loc.cube_id);
                Ok(SeedExportAttempt::Words(words))
            }
            Err(SignerError::InvalidPassword) => match super::unlock_blocking(loc, pin) {
                Ok(PinOutcome::Duress) => {
                    state.record_success(root, loc.cube_id);
                    Ok(SeedExportAttempt::Duress)
                }
                Ok(PinOutcome::Wrong) => {
                    validate().map_err(|error| error.to_string())?;
                    let penalty = state.record_failure(root, loc.cube_id);
                    Ok(SeedExportAttempt::Wrong(penalty))
                }
                // The session and disk disagreed, but the seed on disk opened.
                // Return its words after the same fingerprint/selection check
                // instead of treating a real PIN as an incorrect attempt.
                Ok(PinOutcome::Unlock(signer)) => {
                    let secp =
                        coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::signing_only();
                    if loc.master_signer_fingerprint != Some(signer.fingerprint(&secp)) {
                        return Err(
                            "Cube master seed identity changed during PIN verification".into()
                        );
                    }
                    let words = Zeroizing::new(
                        signer
                            .words()
                            .iter()
                            .map(|word| (*word).to_string())
                            .collect(),
                    );
                    validate().map_err(|error| error.to_string())?;
                    state.record_success(root, loc.cube_id);
                    Ok(SeedExportAttempt::Words(words))
                }
                Err(error) => Err(error.to_string()),
            },
            Err(error) => Err(error.to_string()),
        }
    })
    .map_err(|error| error.to_string())?
}

/// Settings callers capture their Cube at submission. Refuse if its security
/// selection changes while the PIN is being classified, including a rotated
/// duress slot or a re-associated master seed.
pub fn classify_seed_export_cube(
    root: &Path,
    cube: &crate::app::settings::CubeSettings,
    app: crate::app::cache::AppGeneration,
    pin: &str,
    verify: impl FnOnce() -> Result<Vec<String>, SignerError>,
) -> Result<SeedExportCubeAttempt, String> {
    let loc = CubeLocation::new(root, cube);
    let validate = || {
        let directory =
            crate::dir::CoincubeDirectory::new(root.to_path_buf()).network_directory(cube.network);
        let settings = crate::app::settings::Settings::from_file(&directory)
            .map_err(|error| UnlockError::Io(error.to_string()))?;
        let current = settings
            .cubes
            .iter()
            .find(|candidate| candidate.id == cube.id && candidate.network == cube.network)
            .ok_or_else(|| UnlockError::Io("Cube changed during PIN verification".into()))?;
        if current.created_at != cube.created_at
            || current.master_signer_fingerprint != cube.master_signer_fingerprint
            || current.duress_slot_file != cube.duress_slot_file
            || current.is_passkey_cube() != cube.is_passkey_cube()
        {
            return Err(UnlockError::Io(
                "Cube changed during PIN verification".into(),
            ));
        }
        Ok(())
    };
    let result = classify_seed_export(root, &loc, pin, verify, validate)?;
    Ok(match result {
        SeedExportAttempt::Words(words) => SeedExportCubeAttempt::Words(words),
        SeedExportAttempt::Wrong(penalty) => SeedExportCubeAttempt::Wrong(penalty),
        SeedExportAttempt::Locked(remaining) => SeedExportCubeAttempt::Locked(remaining),
        SeedExportAttempt::Duress => {
            let account_id = crate::services::duress::DuressLocalState::load(root)
                .map(|state| state.account_id)
                .unwrap_or(None);
            SeedExportCubeAttempt::Duress(VerifiedSeedExportDuress {
                root: root.to_path_buf(),
                cube_id: cube.id.clone(),
                chain: cube.network,
                app,
                account_id,
                claimed: Arc::new(AtomicBool::new(false)),
            })
        }
    })
}

pub fn classify_pin(root: &Path, loc: &CubeLocation, pin: &str) -> Result<PinAttempt, UnlockError> {
    classify_pin_scoped(root, loc, pin, || Ok(()))
}

/// The fork keeps its PIN-only seed trial and Connect admission in the caller.
/// It still shares the Cube-ID counter with Bitcoin and seed-export trials.
pub fn classify_connect_fork(
    root: &Path,
    loc: &CubeLocation,
    pin: &str,
) -> Result<PinAttempt, UnlockError> {
    classify_connect_fork_with(root, loc.cube_id, || super::unlock_connect_fork(loc, pin))
}

fn classify_connect_fork_with(
    root: &Path,
    cube_id: &str,
    verify: impl FnOnce() -> Result<PinOutcome, UnlockError>,
) -> Result<PinAttempt, UnlockError> {
    classify_pin_with_guard(root, cube_id, verify, || Ok(()))
}

/// The destination variant rechecks selected Cube metadata under the same
/// attempt lock, before and after decryption. A rotated duress marker cannot
/// turn a pending attempt into an ordinary wrong-PIN charge.
pub fn classify_pin_scoped(
    root: &Path,
    loc: &CubeLocation,
    pin: &str,
    validate: impl Fn() -> Result<(), UnlockError>,
) -> Result<PinAttempt, UnlockError> {
    classify_pin_with_guard(
        root,
        loc.cube_id,
        || super::unlock_blocking(loc, pin),
        validate,
    )
}

#[cfg(test)]
fn classify_pin_with(
    root: &Path,
    cube_id: &str,
    verify: impl FnOnce() -> Result<PinOutcome, UnlockError>,
) -> Result<PinAttempt, UnlockError> {
    classify_pin_with_guard(root, cube_id, verify, || Ok(()))
}

fn classify_pin_with_guard(
    root: &Path,
    cube_id: &str,
    verify: impl FnOnce() -> Result<PinOutcome, UnlockError>,
    validate: impl Fn() -> Result<(), UnlockError>,
) -> Result<PinAttempt, UnlockError> {
    with_locked_throttle(root, |state| {
        let remaining = state.remaining_lockout(cube_id);
        if !remaining.is_zero() {
            return Ok(PinAttempt::Locked(remaining));
        }
        validate()?;
        match verify() {
            Ok(PinOutcome::Unlock(signer)) => {
                validate()?;
                state.record_success(root, cube_id);
                Ok(PinAttempt::Classified(PinOutcome::Unlock(signer)))
            }
            Ok(PinOutcome::Duress) => {
                state.record_success(root, cube_id);
                Ok(PinAttempt::Classified(PinOutcome::Duress))
            }
            Ok(PinOutcome::Wrong) => {
                validate()?;
                state.record_failure(root, cube_id);
                Ok(PinAttempt::Classified(PinOutcome::Wrong))
            }
            Err(error) => Err(error),
        }
    })?
}

/// Shared backup/recovery PIN door. The mnemonic stays in the caller's
/// zeroizing result; only wrong-password errors spend a throttle attempt.
pub fn classify_verification<T, E: std::fmt::Display>(
    root: &Path,
    cube_id: &str,
    verify: impl FnOnce() -> Result<T, E>,
    is_wrong: impl FnOnce(&E) -> bool,
) -> Result<Result<T, String>, UnlockError> {
    with_locked_throttle(root, |state| {
        let remaining = state.remaining_lockout(cube_id);
        if !remaining.is_zero() {
            return Err(lockout_message(remaining));
        }
        match verify() {
            Ok(value) => {
                state.record_success(root, cube_id);
                Ok(value)
            }
            Err(error) if !is_wrong(&error) => Err(error.to_string()),
            Err(_) => {
                let penalty = state.record_failure(root, cube_id);
                Err(if penalty.is_zero() {
                    "Incorrect PIN. Please try again.".to_string()
                } else {
                    lockout_message(penalty)
                })
            }
        }
    })
}

fn with_locked_throttle<T>(
    root: &Path,
    action: impl FnOnce(&mut ThrottleState) -> T,
) -> Result<T, UnlockError> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(LOCK_NAME))
        .map_err(|e| UnlockError::Io(e.to_string()))?;
    file.lock_exclusive()
        .map_err(|e| UnlockError::Io(e.to_string()))?;

    let mut state = ThrottleState::load(root);
    let answer = action(&mut state);
    let _ = FileExt::unlock(&file);
    Ok(answer)
}

/// Delay after the Nth consecutive failure: 0, 0, 1s, 2s, 4s, 8s… capped.
///
/// The first two failures are free — a mistyped PIN is the overwhelmingly
/// common case and punishing it teaches users nothing.
const FREE_ATTEMPTS: u32 = 2;
const MAX_DELAY: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ThrottleState {
    /// Consecutive failures across the whole device, per Cube.
    #[serde(default)]
    failures: std::collections::BTreeMap<String, CubeThrottle>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct CubeThrottle {
    consecutive_failures: u32,
    /// Unix seconds of the last failure. Absolute rather than an `Instant` so
    /// the penalty survives a restart — otherwise "quit and relaunch" is the
    /// bypass.
    last_failure_unix: u64,
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn path(root: &Path) -> PathBuf {
    root.join(FILE_NAME)
}

impl ThrottleState {
    /// The last attempt's full penalty, before wall-clock expiry. The PIN UI
    /// shows the same message it did when it recorded that attempt itself.
    pub fn current_penalty(&self, cube_id: &str) -> Duration {
        self.failures
            .get(cube_id)
            .map(|entry| penalty_for(entry.consecutive_failures))
            .unwrap_or(Duration::ZERO)
    }
    /// Load, mapping a missing file to the empty state.
    ///
    /// A **corrupt** file also maps to empty rather than erroring. That is the
    /// permissive direction, and it is the right one here: this is not a
    /// security control, and failing closed would mean an unparseable counter
    /// file locks a user out of their own wallet permanently.
    pub fn load(root: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path(root)) else {
            return Self::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_default()
    }

    fn save(&self, root: &Path) {
        let Ok(bytes) = serde_json::to_vec_pretty(self) else {
            return;
        };
        if let Err(e) = std::fs::write(path(root), bytes) {
            tracing::debug!("could not persist the unlock throttle: {e}");
        }
    }

    /// How much longer this Cube must wait before another attempt is accepted.
    /// `Duration::ZERO` when it may try now.
    pub fn remaining_lockout(&self, cube_id: &str) -> Duration {
        self.remaining_lockout_at(cube_id, now_unix())
    }

    fn remaining_lockout_at(&self, cube_id: &str, now_unix: u64) -> Duration {
        let Some(entry) = self.failures.get(cube_id) else {
            return Duration::ZERO;
        };
        let penalty = penalty_for(entry.consecutive_failures);
        if penalty.is_zero() {
            return Duration::ZERO;
        }
        let elapsed = now_unix.saturating_sub(entry.last_failure_unix);
        penalty.saturating_sub(Duration::from_secs(elapsed))
    }

    /// Record a wrong PIN and return the new lockout.
    pub fn record_failure(&mut self, root: &Path, cube_id: &str) -> Duration {
        let entry = self.failures.entry(cube_id.to_string()).or_default();
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        entry.last_failure_unix = now_unix();
        let penalty = penalty_for(entry.consecutive_failures);
        self.save(root);
        penalty
    }

    /// Clear this Cube's counter after a successful unlock.
    ///
    /// Also called on a **duress** activation: the counter must not survive as
    /// evidence that someone was guessing, and the Cube is being wiped anyway.
    pub fn record_success(&mut self, root: &Path, cube_id: &str) {
        if self.failures.remove(cube_id).is_some() {
            self.save(root);
        }
    }
}

/// 1s, 2s, 4s, 8s… after the free attempts, capped at [`MAX_DELAY`].
fn penalty_for(consecutive_failures: u32) -> Duration {
    if consecutive_failures <= FREE_ATTEMPTS {
        return Duration::ZERO;
    }
    let step = consecutive_failures - FREE_ATTEMPTS - 1;
    // Saturate rather than overflow: 2^32 seconds is not a number we want to
    // reach by shifting.
    let secs = 1u64.checked_shl(step.min(32)).unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(MAX_DELAY)
}

/// User-facing text for a remaining lockout.
pub fn lockout_message(remaining: Duration) -> String {
    let secs = remaining.as_secs().max(1);
    if secs < 60 {
        format!("Too many incorrect PINs. Try again in {secs}s.")
    } else {
        let mins = secs.div_ceil(60);
        format!("Too many incorrect PINs. Try again in {mins} minute(s).")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn synthetic_seed(root: &Path, cube: &mut crate::app::settings::CubeSettings) {
        use coincube_core::miniscript::bitcoin::{secp256k1::Secp256k1, Network};
        use coincube_core::signer::{MasterSigner, MASTER_SEED_LABEL};

        let signer = MasterSigner::generate(Network::Bitcoin).unwrap();
        let secp = Secp256k1::signing_only();
        cube.master_signer_fingerprint = Some(signer.fingerprint(&secp));
        signer
            .store_encrypted(
                root,
                Network::Bitcoin,
                &secp,
                Some((
                    format!("{}{}", MASTER_SEED_LABEL, cube.created_at),
                    cube.created_at,
                )),
                "1234",
                &cube.id,
                None,
            )
            .unwrap();
    }

    #[test]
    fn seed_export_trial_classifies_real_wrong_duress_and_lockout() {
        use crate::{app::settings::CubeSettings, chain::ChainId};

        let root = tmp("seed-export-outcomes");
        let mut cube =
            CubeSettings::new_with_raw_id("export-cube".into(), "Export".into(), ChainId::Bitcoin);
        synthetic_seed(&root, &mut cube);
        let marker = super::super::marker::new_file_name(cube.created_at);
        super::super::marker::write(&root, ChainId::Bitcoin, &cube.id, &marker, "9999", None)
            .unwrap();
        cube.duress_slot_file = Some(marker);
        let loc = CubeLocation::new(&root, &cube);
        let verify = |pin: &str| {
            super::super::open_seed_by_fingerprint(
                &root,
                ChainId::Bitcoin,
                cube.master_signer_fingerprint.unwrap(),
                pin,
                &cube.id,
            )
            .map(|signer| {
                signer
                    .words()
                    .iter()
                    .map(|word| (*word).to_string())
                    .collect()
            })
        };
        assert!(matches!(
            classify_seed_export(&root, &loc, "4321", || verify("4321"), || Ok(())).unwrap(),
            SeedExportAttempt::Wrong(Duration::ZERO)
        ));
        assert!(matches!(
            classify_seed_export(&root, &loc, "1234", || verify("1234"), || Ok(())).unwrap(),
            SeedExportAttempt::Words(_)
        ));
        assert!(matches!(
            classify_seed_export(&root, &loc, "9999", || verify("9999"), || Ok(())).unwrap(),
            SeedExportAttempt::Duress
        ));
        for _ in 0..3 {
            assert!(matches!(
                classify_seed_export(&root, &loc, "4321", || verify("4321"), || Ok(())).unwrap(),
                SeedExportAttempt::Wrong(_)
            ));
        }
        assert!(matches!(
            classify_seed_export(
                &root,
                &loc,
                "1234",
                || panic!("lockout must prevent seed export"),
                || Ok(())
            )
            .unwrap(),
            SeedExportAttempt::Locked(_)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn seed_export_fallback_refuses_a_substituted_signer() {
        use crate::{app::settings::CubeSettings, chain::ChainId};
        use coincube_core::miniscript::bitcoin::{secp256k1::Secp256k1, Network};
        use coincube_core::signer::MasterSigner;

        let root = tmp("seed-export-substitution");
        let mut cube =
            CubeSettings::new_with_raw_id("export-cube".into(), "Export".into(), ChainId::Bitcoin);
        synthetic_seed(&root, &mut cube);
        let actual = cube.master_signer_fingerprint.unwrap();
        let expected = MasterSigner::generate(Network::Bitcoin)
            .unwrap()
            .fingerprint(&Secp256k1::signing_only());
        let folder = super::super::seed_folder(&root, ChainId::Bitcoin);
        let path = std::fs::read_dir(&folder)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let changed_name = path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .replace(&actual.to_string(), &expected.to_string());
        std::fs::rename(&path, folder.join(changed_name)).unwrap();
        cube.master_signer_fingerprint = Some(expected);
        let loc = CubeLocation::new(&root, &cube);
        let result = classify_seed_export(
            &root,
            &loc,
            "1234",
            || Err(SignerError::InvalidPassword),
            || Ok(()),
        );
        assert!(matches!(result, Err(message) if message.contains("identity changed")));
        let counters = serde_json::to_value(ThrottleState::load(&root)).unwrap();
        assert!(counters["failures"].get("export-cube").is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn confirmed_duress_survives_slot_rotation_during_trial_but_words_do_not() {
        use crate::{app::settings::CubeSettings, chain::ChainId, dir::CoincubeDirectory};
        let root = tmp("seed-export-rotation");
        let mut cube =
            CubeSettings::new_with_raw_id("export-cube".into(), "Export".into(), ChainId::Bitcoin);
        synthetic_seed(&root, &mut cube);
        let marker = super::super::marker::new_file_name(cube.created_at);
        super::super::marker::write(&root, ChainId::Bitcoin, &cube.id, &marker, "9999", None)
            .unwrap();
        cube.duress_slot_file = Some(marker);
        let directory = CoincubeDirectory::new(root.clone()).network_directory(ChainId::Bitcoin);
        std::fs::create_dir_all(directory.path()).unwrap();
        let path = directory
            .path()
            .join(crate::app::settings::SETTINGS_FILE_NAME);
        let mut settings = crate::app::settings::Settings::default();
        settings.cubes.push(cube.clone());
        std::fs::write(&path, serde_json::to_vec(&settings).unwrap()).unwrap();
        let rotate = || {
            let mut settings = crate::app::settings::Settings::from_file(&directory).unwrap();
            settings.cubes[0].duress_slot_file = Some("rotated-marker".into());
            std::fs::write(&path, serde_json::to_vec(&settings).unwrap()).unwrap();
        };
        let duress = classify_seed_export_cube(
            &root,
            &cube,
            crate::app::cache::AppGeneration::next(),
            "9999",
            || {
                rotate();
                Err(SignerError::InvalidPassword)
            },
        )
        .unwrap();
        assert!(matches!(duress, SeedExportCubeAttempt::Duress(_)));
        // A real seed result from the old selection cannot become exportable.
        let stale_words = classify_seed_export_cube(
            &root,
            &cube,
            crate::app::cache::AppGeneration::next(),
            "1234",
            || panic!("rotated selection must fail before seed decrypt"),
        );
        assert!(matches!(stale_words, Err(message) if message.contains("Cube changed")));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn remaining_at_last_failure(state: &ThrottleState, cube_id: &str) -> Duration {
        let last_failure_unix = state
            .failures
            .get(cube_id)
            .expect("the test must record a failure first")
            .last_failure_unix;
        state.remaining_lockout_at(cube_id, last_failure_unix)
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "coincube-throttle-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn concurrent_pin_trials_share_one_persisted_counter() {
        let root = tmp("concurrent-attempts");
        let barrier = Arc::new(Barrier::new(4));
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let root = root.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    classify_pin_with(&root, "cube-a", || Ok(PinOutcome::Wrong)).unwrap()
                })
            })
            .collect();
        barrier.wait();
        for handle in handles {
            assert!(matches!(
                handle.join().unwrap(),
                PinAttempt::Classified(PinOutcome::Wrong)
            ));
        }
        let state = ThrottleState::load(&root);
        assert_eq!(state.failures["cube-a"].consecutive_failures, 3);
        assert!(matches!(
            classify_pin_with(&root, "cube-a", || panic!("locked trial must not decrypt")).unwrap(),
            PinAttempt::Locked(_)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_and_unlock_trials_share_the_lock_and_counter() {
        let root = tmp("cross-surface");
        let barrier = Arc::new(Barrier::new(4));
        let handles: Vec<_> = (0..3)
            .map(|i| {
                let root = root.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    if i == 0 {
                        let answer =
                            classify_pin_with(&root, "cube-a", || Ok(PinOutcome::Wrong)).unwrap();
                        assert!(matches!(answer, PinAttempt::Classified(PinOutcome::Wrong)));
                    } else {
                        let answer = classify_verification::<(), _>(
                            &root,
                            "cube-a",
                            || Err("wrong"),
                            |_| true,
                        )
                        .unwrap();
                        assert!(answer.is_err());
                    }
                })
            })
            .collect();
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(
            ThrottleState::load(&root).failures["cube-a"].consecutive_failures,
            3
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bitcoin_and_fork_trials_share_one_atomic_cube_counter() {
        let root = tmp("cross-chain-fork");
        let barrier = Arc::new(Barrier::new(4));
        let handles: Vec<_> = (0..3)
            .map(|i| {
                let root = root.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let attempt = if i == 0 {
                        // Fork seed decryption remains distinct, but trial and
                        // counter update must use the same lock as Bitcoin.
                        classify_connect_fork_with(&root, "shared-cube", || Ok(PinOutcome::Wrong))
                    } else {
                        classify_pin_with(&root, "shared-cube", || Ok(PinOutcome::Wrong))
                    }
                    .unwrap();
                    assert!(matches!(attempt, PinAttempt::Classified(PinOutcome::Wrong)));
                })
            })
            .collect();
        barrier.wait();
        for handle in handles {
            handle.join().unwrap();
        }
        assert_eq!(
            ThrottleState::load(&root).failures["shared-cube"].consecutive_failures,
            3
        );
        assert!(matches!(
            classify_connect_fork_with(&root, "shared-cube", || {
                panic!("lockout must refuse before fork seed decryption")
            })
            .unwrap(),
            PinAttempt::Locked(_)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn metadata_rotation_refuses_stale_pin_without_a_wrong_attempt() {
        let root = tmp("metadata-rotation");
        let checks = std::cell::Cell::new(0);
        let answer = classify_pin_with_guard(
            &root,
            "cube-a",
            || Ok(PinOutcome::Wrong),
            || {
                checks.set(checks.get() + 1);
                if checks.get() == 2 {
                    Err(UnlockError::Io("rotated".into()))
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(answer, Err(UnlockError::Io(_))));
        assert!(!ThrottleState::load(&root).failures.contains_key("cube-a"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn duress_clears_the_counter_and_operational_error_does_not_charge_it() {
        let root = tmp("equivalent-outcomes");
        for _ in 0..2 {
            assert!(matches!(
                classify_pin_with(&root, "cube-a", || Ok(PinOutcome::Wrong)).unwrap(),
                PinAttempt::Classified(PinOutcome::Wrong)
            ));
        }
        let error = classify_pin_with(&root, "cube-a", || Err(UnlockError::NoPinConfigured));
        assert!(matches!(error, Err(UnlockError::NoPinConfigured)));
        assert_eq!(
            ThrottleState::load(&root).failures["cube-a"].consecutive_failures,
            2
        );
        assert!(matches!(
            classify_pin_with(&root, "cube-a", || Ok(PinOutcome::Duress)).unwrap(),
            PinAttempt::Classified(PinOutcome::Duress)
        ));
        assert!(!ThrottleState::load(&root).failures.contains_key("cube-a"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn first_attempts_are_free_then_it_escalates() {
        assert_eq!(penalty_for(0), Duration::ZERO);
        assert_eq!(penalty_for(1), Duration::ZERO);
        assert_eq!(penalty_for(2), Duration::ZERO);
        assert_eq!(penalty_for(3), Duration::from_secs(1));
        assert_eq!(penalty_for(4), Duration::from_secs(2));
        assert_eq!(penalty_for(5), Duration::from_secs(4));
        assert_eq!(penalty_for(6), Duration::from_secs(8));
    }

    #[test]
    fn the_delay_is_capped_and_never_overflows() {
        // A determined guesser must not be able to push this into a value that
        // wraps, or into a lockout measured in years.
        assert_eq!(penalty_for(40), MAX_DELAY);
        assert_eq!(penalty_for(u32::MAX), MAX_DELAY);
    }

    #[test]
    fn failures_accumulate_and_success_clears_them() {
        let root = tmp("accumulate");
        let mut st = ThrottleState::load(&root);
        assert_eq!(st.remaining_lockout("cube-a"), Duration::ZERO);

        for _ in 0..3 {
            st.record_failure(&root, "cube-a");
        }
        assert!(remaining_at_last_failure(&st, "cube-a") > Duration::ZERO);

        st.record_success(&root, "cube-a");
        assert_eq!(st.remaining_lockout("cube-a"), Duration::ZERO);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_public_lockout_uses_current_time_and_expires() {
        let root = tmp("expires");
        let mut st = ThrottleState::load(&root);
        for _ in 0..3 {
            st.record_failure(&root, "cube-a");
        }
        st.failures
            .get_mut("cube-a")
            .expect("the test must record a failure first")
            .last_failure_unix = now_unix().saturating_sub(2);

        assert_eq!(st.remaining_lockout("cube-a"), Duration::ZERO);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_penalty_survives_a_restart() {
        // Quitting and relaunching must not reset the counter — otherwise it
        // is a two-keystroke bypass.
        let root = tmp("restart");
        let mut st = ThrottleState::load(&root);
        for _ in 0..4 {
            st.record_failure(&root, "cube-a");
        }
        let reloaded = ThrottleState::load(&root);
        assert!(remaining_at_last_failure(&reloaded, "cube-a") > Duration::ZERO);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Guesses must accumulate across *every* surface that checks the PIN, not
    /// just the unlock screen.
    ///
    /// The Settings → Backup Master Seed and Recovery Kit flows also decrypt the
    /// seed with a user-supplied PIN. They sit behind an unlocked Cube, so they
    /// are not the laptop-thief case — but they are the route to the
    /// *permanent* secret, and leaving either unthrottled would mean the
    /// counter could be sidestepped by guessing at a different door.
    #[test]
    fn the_counter_is_shared_across_pin_surfaces() {
        let root = tmp("shared");

        // Three failures anywhere put the Cube into lockout...
        let mut st = ThrottleState::load(&root);
        for _ in 0..3 {
            st.record_failure(&root, "cube-a");
        }

        // ...and a *different* surface, loading the state fresh, sees it.
        let seen_elsewhere = ThrottleState::load(&root);
        let remaining = remaining_at_last_failure(&seen_elsewhere, "cube-a");
        assert!(
            remaining > Duration::ZERO,
            "a second PIN surface would have offered unlimited fresh guesses"
        );

        // A success anywhere clears it for everyone.
        ThrottleState::load(&root).record_success(&root, "cube-a");
        assert_eq!(
            ThrottleState::load(&root).remaining_lockout("cube-a"),
            Duration::ZERO
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cubes_are_throttled_independently() {
        let root = tmp("independent");
        let mut st = ThrottleState::load(&root);
        for _ in 0..5 {
            st.record_failure(&root, "cube-a");
        }
        assert!(remaining_at_last_failure(&st, "cube-a") > Duration::ZERO);
        assert_eq!(st.remaining_lockout("cube-b"), Duration::ZERO);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_corrupt_counter_file_does_not_lock_the_user_out() {
        // Failing closed here would mean an unparseable JSON file bricks a
        // wallet. This is hygiene, not a control — it fails open on purpose.
        let root = tmp("corrupt");
        std::fs::write(path(&root), b"{not json").unwrap();
        let st = ThrottleState::load(&root);
        assert_eq!(st, ThrottleState::default());
        assert_eq!(st.remaining_lockout("cube-a"), Duration::ZERO);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lockout_copy_reads_sensibly_at_both_scales() {
        assert!(lockout_message(Duration::from_secs(4)).contains("4s"));
        assert!(lockout_message(Duration::from_secs(120)).contains("2 minute"));
        // Sub-second remainders still say something, never "0s".
        assert!(lockout_message(Duration::from_millis(200)).contains("1s"));
    }
}
