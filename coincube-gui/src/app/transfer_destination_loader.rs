//! Restricted destination Vault admission for a future cross-Cube Transfer.
//! Neither a signer nor a PRF result enters an Iced message or the global
//! session. This module cannot sign, construct a source spend, or broadcast.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use coincube_core::miniscript::bitcoin::{bip32::Fingerprint, Network};
use coincube_core::signer::MasterSigner;
use coincubed::config::Config;
use zeroize::Zeroizing;

use super::{
    cache::AppGeneration,
    settings::{CubeSettings, Settings, WalletId, WalletSettings},
    transfer_destination::{OwnedDestinationResource, Source, WalletIdentity},
    wallet::{descriptor_id_fingerprint, Wallet},
};
use crate::{
    chain::ChainId,
    daemon::{client, Daemon},
    dir::CoincubeDirectory,
    services::unlock::{self, throttle::PinAttempt, PinOutcome},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refusal {
    Stale,
    Unsupported,
    IdentityChanged,
    BackendUnavailable,
    WrongCredential,
    Duress,
    Locked,
}

/// One exact local catalog snapshot. Re-read before and after every async
/// backend call; no caller-provided Cube label or address is trusted.
fn selected(
    root: &CoincubeDirectory,
    cube_id: &str,
    wallet_id: &WalletId,
) -> Result<(CubeSettings, WalletSettings), Refusal> {
    let settings = Settings::from_file(&root.network_directory(ChainId::Bitcoin))
        .map_err(|_| Refusal::BackendUnavailable)?;
    let cube = settings
        .cubes
        .into_iter()
        .find(|cube| cube.id == cube_id)
        .ok_or(Refusal::IdentityChanged)?;
    if cube.network != ChainId::Bitcoin
        || cube.bitcoin_import.is_some()
        || cube.vault_wallet_id.as_ref() != Some(wallet_id)
        || cube.master_signer_fingerprint.is_none()
        || cube.vault_fingerprint.as_deref().is_none_or(str::is_empty)
    {
        return Err(Refusal::Unsupported);
    }
    let wallet = settings
        .wallets
        .into_iter()
        .find(|wallet| wallet.wallet_id() == *wallet_id)
        .ok_or(Refusal::IdentityChanged)?;
    Ok((cube, wallet))
}

fn same_selection(expected: &CubeSettings, current: &CubeSettings) -> bool {
    expected.id == current.id
        && expected.network == current.network
        && expected.vault_wallet_id == current.vault_wallet_id
        && expected.vault_fingerprint == current.vault_fingerprint
        && expected.master_signer_fingerprint == current.master_signer_fingerprint
        && expected
            .passkey_metadata
            .as_ref()
            .map(|meta| (&meta.rp_id, &meta.credential_id))
            == current
                .passkey_metadata
                .as_ref()
                .map(|meta| (&meta.rp_id, &meta.credential_id))
}

fn operation_live(
    expected_source: &Source,
    current_source: &Source,
    expected_generation: AppGeneration,
    result_generation: AppGeneration,
    cancelled: &AtomicBool,
) -> bool {
    !cancelled.load(Ordering::Acquire)
        && expected_source == current_source
        && expected_generation == result_generation
}

pub(crate) struct ScopedDestination {
    root: CoincubeDirectory,
    source: Source,
    cube: CubeSettings,
    wallet_id: WalletId,
    generation: AppGeneration,
    cancelled: Arc<AtomicBool>,
    pin_busy: Arc<AtomicBool>,
    pin_signer: Arc<Mutex<Option<MasterSigner>>>,
    verified: bool,
    #[cfg(target_os = "macos")]
    ceremony: Option<crate::services::passkey::macos::NativePasskeyCeremony>,
    backend: Option<BoundBackend>,
}

impl ScopedDestination {
    pub(crate) fn new(
        root: CoincubeDirectory,
        source: Source,
        destination_cube_id: &str,
        wallet_id: WalletId,
    ) -> Result<Self, Refusal> {
        if source.cube_id == destination_cube_id
            || source.chain != ChainId::Bitcoin
            || !matches!(source.wallet, WalletIdentity::Vault(_))
        {
            return Err(Refusal::Unsupported);
        }
        let (cube, _) = selected(&root, destination_cube_id, &wallet_id)?;
        Ok(Self {
            root,
            source,
            cube,
            wallet_id,
            generation: AppGeneration::next(),
            cancelled: Arc::new(AtomicBool::new(false)),
            pin_busy: Arc::new(AtomicBool::new(false)),
            pin_signer: Arc::new(Mutex::new(None)),
            verified: false,
            #[cfg(target_os = "macos")]
            ceremony: None,
            backend: None,
        })
    }

    pub(crate) fn generation(&self) -> AppGeneration {
        self.generation
    }

    fn current(&self, source: &Source) -> Result<(), Refusal> {
        if !operation_live(
            &self.source,
            source,
            self.generation,
            self.generation,
            &self.cancelled,
        ) {
            return Err(Refusal::Stale);
        }
        let (cube, _) = selected(&self.root, &self.cube.id, &self.wallet_id)?;
        if !same_selection(&self.cube, &cube) {
            return Err(Refusal::IdentityChanged);
        }
        Ok(())
    }

    /// The returned job is `Send` and runs on the blocking pool. Its result
    /// carries a verdict only; the signer stays in this flow's private slot.
    pub(crate) fn begin_pin(
        &self,
        source: &Source,
        pin: Zeroizing<String>,
    ) -> Result<PinRequest, Refusal> {
        self.current(source)?;
        if self.cube.is_passkey_cube() || self.verified {
            return Err(Refusal::Unsupported);
        }
        if self
            .pin_busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Refusal::Stale);
        }
        Ok(PinRequest {
            root: self.root.clone(),
            cube: self.cube.clone(),
            pin,
            generation: self.generation,
            cancelled: Arc::clone(&self.cancelled),
            busy: Arc::clone(&self.pin_busy),
            signer_slot: Arc::clone(&self.pin_signer),
        })
    }

    pub(crate) fn accept_pin(&mut self, source: &Source, result: PinCheck) -> Result<(), Refusal> {
        // Duress remains an emergency signal if the source locked or switched
        // during Argon2. The caller must activate the existing orchestrator.
        if matches!(&result.verdict, Ok(PinVerdict::Duress)) {
            self.revoke();
            return Err(Refusal::Duress);
        }
        if let Err(error) = self.current(source) {
            self.revoke();
            return Err(error);
        }
        if result.generation != self.generation {
            return Err(Refusal::Stale);
        }
        match result.verdict? {
            PinVerdict::Locked => Err(Refusal::Locked),
            PinVerdict::Wrong => Err(Refusal::WrongCredential),
            PinVerdict::Duress => {
                self.revoke();
                Err(Refusal::Duress)
            }
            PinVerdict::Unlock => {
                let signer = self
                    .pin_signer
                    .lock()
                    .map_err(|_| Refusal::BackendUnavailable)?
                    .take()
                    .ok_or(Refusal::Stale)?;
                self.accept_signer(source, signer)
            }
        }
    }

    fn accept_signer(&mut self, source: &Source, signer: MasterSigner) -> Result<(), Refusal> {
        self.current(source)?;
        let secp = coincube_core::miniscript::bitcoin::secp256k1::Secp256k1::signing_only();
        let fingerprint: Fingerprint = signer.fingerprint(&secp);
        if self.cube.master_signer_fingerprint != Some(fingerprint) {
            return Err(Refusal::IdentityChanged);
        }
        // No signer is needed to receive. Dropping it scrubs its mnemonic and
        // xpriv; only this operation's verified bit remains.
        drop(signer);
        self.verified = true;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn begin_passkey(&mut self, source: &Source) -> Result<(), Refusal> {
        use base64::Engine;
        self.current(source)?;
        if !self.cube.is_passkey_cube()
            || self.ceremony.is_some()
            || self.pin_busy.load(Ordering::Acquire)
            || self.verified
        {
            return Err(Refusal::Unsupported);
        }
        let meta = self
            .cube
            .passkey_metadata
            .as_ref()
            .ok_or(Refusal::Unsupported)?;
        let credential_id = base64::engine::general_purpose::STANDARD
            .decode(&meta.credential_id)
            .map_err(|_| Refusal::Unsupported)?;
        self.ceremony = Some(
            crate::services::passkey::macos::NativePasskeyCeremony::authenticate(
                &meta.rp_id,
                &credential_id,
            )
            .map_err(|_| Refusal::BackendUnavailable)?,
        );
        Ok(())
    }

    /// Poll on the UI thread; consume the PRF inline, never in a cloned event.
    #[cfg(target_os = "macos")]
    pub(crate) fn poll_passkey(&mut self, source: &Source) -> Option<Result<(), Refusal>> {
        use crate::services::passkey::macos::NativeOutcome;
        if let Err(error) = self.current(source) {
            self.revoke();
            return Some(Err(error));
        }
        let outcome = self.ceremony.as_ref()?.try_recv()?;
        self.ceremony = None;
        Some(match outcome {
            NativeOutcome::Authenticated { prf_output } => {
                MasterSigner::from_prf_output(Network::Bitcoin, &prf_output)
                    .map_err(|_| Refusal::WrongCredential)
                    .and_then(|signer| self.accept_signer(source, signer))
            }
            NativeOutcome::Registered { .. } | NativeOutcome::Failed(_) => {
                Err(Refusal::WrongCredential)
            }
        })
    }

    /// Creates a separately runnable, cancellation-aware read-only attachment.
    /// A future UI may dispatch it but must call `accept_backend` on return.
    pub(crate) fn request_backend(&self, source: &Source) -> Result<BackendRequest, Refusal> {
        self.current(source)?;
        if !self.verified {
            return Err(Refusal::WrongCredential);
        }
        Ok(BackendRequest {
            root: self.root.clone(),
            cube: self.cube.clone(),
            wallet_id: self.wallet_id.clone(),
            generation: self.generation,
            cancelled: Arc::clone(&self.cancelled),
        })
    }

    pub(crate) fn accept_backend(
        &mut self,
        source: &Source,
        generation: AppGeneration,
        backend: BoundBackend,
    ) -> Result<(), Refusal> {
        self.current(source)?;
        if !self.verified
            || !operation_live(
                &self.source,
                source,
                self.generation,
                generation,
                &self.cancelled,
            )
            || backend.wallet_id != self.wallet_id
        {
            return Err(Refusal::Stale);
        }
        self.backend = Some(backend);
        Ok(())
    }

    pub(crate) fn revoke(&mut self) {
        self.cancelled.store(true, Ordering::Release);
        self.verified = false;
        if let Ok(mut slot) = self.pin_signer.lock() {
            *slot = None;
        }
        #[cfg(target_os = "macos")]
        {
            self.ceremony = None;
        }
        self.backend = None;
    }
}

/// The only result permitted through an Iced message is a classification.
/// There is no signer, PIN, or PRF output here.
#[derive(Debug, Clone)]
pub(crate) struct PinCheck {
    generation: AppGeneration,
    verdict: Result<PinVerdict, Refusal>,
}

#[derive(Debug, Clone)]
enum PinVerdict {
    Unlock,
    Duress,
    Wrong,
    Locked,
}

pub(crate) struct PinRequest {
    root: CoincubeDirectory,
    cube: CubeSettings,
    pin: Zeroizing<String>,
    generation: AppGeneration,
    cancelled: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
    signer_slot: Arc<Mutex<Option<MasterSigner>>>,
}

impl PinRequest {
    pub(crate) fn run(self) -> PinCheck {
        let verdict = if self.cancelled.load(Ordering::Acquire) {
            Err(Refusal::Stale)
        } else {
            let loc = unlock::CubeLocation::new(self.root.path(), &self.cube);
            match unlock::throttle::classify_pin(self.root.path(), &loc, &self.pin) {
                Ok(PinAttempt::Locked(_)) => Ok(PinVerdict::Locked),
                Ok(PinAttempt::Classified(PinOutcome::Wrong)) => Ok(PinVerdict::Wrong),
                Ok(PinAttempt::Classified(PinOutcome::Duress)) => Ok(PinVerdict::Duress),
                Ok(PinAttempt::Classified(PinOutcome::Unlock(signer))) => {
                    match self.signer_slot.lock() {
                        Ok(mut slot) if !self.cancelled.load(Ordering::Acquire) => {
                            *slot = Some(*signer);
                            Ok(PinVerdict::Unlock)
                        }
                        _ => Err(Refusal::Stale),
                    }
                }
                Err(_) => Err(Refusal::BackendUnavailable),
            }
        };
        PinCheck {
            generation: self.generation,
            verdict,
        }
    }
}

impl Drop for PinRequest {
    fn drop(&mut self) {
        self.busy.store(false, Ordering::Release);
    }
}

impl Drop for ScopedDestination {
    fn drop(&mut self) {
        self.revoke();
    }
}

pub(crate) struct BackendRequest {
    root: CoincubeDirectory,
    cube: CubeSettings,
    wallet_id: WalletId,
    generation: AppGeneration,
    cancelled: Arc<AtomicBool>,
}

impl BackendRequest {
    pub(crate) fn generation(&self) -> AppGeneration {
        self.generation
    }

    pub(crate) async fn run(self) -> Result<BoundBackend, Refusal> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(Refusal::Stale);
        }
        let (current, wallet) = selected(&self.root, &self.cube.id, &self.wallet_id)?;
        if !same_selection(&self.cube, &current) {
            return Err(Refusal::IdentityChanged);
        }
        // This restricted path attaches to an already running, wallet-scoped
        // local backend. It does not start a daemon or share the source socket.
        let data_dir = self
            .root
            .network_directory(ChainId::Bitcoin)
            .coincubed_data_directory(&self.wallet_id);
        let config = Config::from_file(Some(data_dir.path().join("daemon.toml")))
            .map_err(|_| Refusal::BackendUnavailable)?;
        let expected_dir =
            std::path::absolute(data_dir.path()).map_err(|_| Refusal::BackendUnavailable)?;
        if config.bitcoin_config.chain != ChainId::Bitcoin
            || config
                .data_directory()
                .is_none_or(|dir| !crate::chain::same_path(dir.path(), &expected_dir))
        {
            return Err(Refusal::IdentityChanged);
        }
        let socket = data_dir.coincubed_rpc_socket_path();
        let daemon: Arc<dyn Daemon + Send + Sync> = Arc::new(client::Coincubed::new(
            client::jsonrpc::JsonRPCClient::new(socket),
        ));
        let info = daemon
            .get_info()
            .await
            .map_err(|_| Refusal::BackendUnavailable)?;
        if self.cancelled.load(Ordering::Acquire) {
            return Err(Refusal::Stale);
        }
        let (latest, latest_wallet) = selected(&self.root, &self.cube.id, &self.wallet_id)?;
        if !same_selection(&current, &latest) || wallet.wallet_id() != latest_wallet.wallet_id() {
            return Err(Refusal::IdentityChanged);
        }
        if info.chain_id != Some(ChainId::Bitcoin) || info.network != Network::Bitcoin {
            return Err(Refusal::IdentityChanged);
        }
        let descriptor = info
            .descriptors
            .main
            .as_vault()
            .ok_or(Refusal::Unsupported)?;
        if config.main_descriptor != info.descriptors.main {
            return Err(Refusal::IdentityChanged);
        }
        let fingerprint = descriptor_id_fingerprint(descriptor).to_string();
        if latest.vault_fingerprint.as_deref() != Some(fingerprint.as_str()) {
            return Err(Refusal::IdentityChanged);
        }
        Wallet::new(descriptor.clone())
            .load_from_settings(latest_wallet)
            .map_err(|_| Refusal::IdentityChanged)?;
        Ok(BoundBackend {
            wallet_id: self.wallet_id,
            _daemon: daemon,
            _lease: BackendLease(None),
        })
    }
}

/// An attached external service is dropped, never sent `stop`. No daemon is
/// spawned by this restricted path; future owned resources require an explicit
/// close implementation before they can be placed in the owner.
pub(crate) struct BoundBackend {
    wallet_id: WalletId,
    _daemon: Arc<dyn Daemon + Send + Sync>,
    _lease: BackendLease,
}

struct BackendLease(Option<Box<dyn OwnedDestinationResource>>);

impl Drop for BackendLease {
    fn drop(&mut self) {
        if let Some(resource) = self.0.take() {
            resource.close();
        }
        // The JSON-RPC client merely attached to an external process. Dropping
        // its Arc closes our handle and never invokes Daemon::stop.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn source() -> Source {
        Source {
            cube_id: "source".into(),
            chain: ChainId::Bitcoin,
            wallet: WalletIdentity::Vault(WalletId::new("source-id".into(), Some(1))),
            app: AppGeneration::next(),
        }
    }

    #[test]
    fn stale_source_generation_and_cancellation_refuse_a_result() {
        let original = source();
        let generation = AppGeneration::next();
        let cancelled = AtomicBool::new(false);
        assert!(operation_live(
            &original, &original, generation, generation, &cancelled
        ));
        let mut switched = original.clone();
        switched.app = AppGeneration::next();
        assert!(!operation_live(
            &original, &switched, generation, generation, &cancelled
        ));
        assert!(!operation_live(
            &original,
            &original,
            generation,
            AppGeneration::next(),
            &cancelled
        ));
        cancelled.store(true, Ordering::Release);
        assert!(!operation_live(
            &original, &original, generation, generation, &cancelled
        ));
    }

    #[test]
    fn selected_cube_mutation_invalidates_its_verification() {
        let mut selected = CubeSettings::new_with_raw_id(
            "destination".into(),
            "Destination".into(),
            ChainId::Bitcoin,
        );
        selected.vault_wallet_id = Some(WalletId::new("selected".into(), Some(2)));
        let mut changed = selected.clone();
        assert!(same_selection(&selected, &changed));
        changed.vault_wallet_id = Some(WalletId::new("rotated".into(), Some(2)));
        assert!(!same_selection(&selected, &changed));
        changed = selected.clone();
        changed.network = ChainId::Testnet;
        assert!(!same_selection(&selected, &changed));
    }

    struct CountClose(Arc<AtomicUsize>);

    impl OwnedDestinationResource for CountClose {
        fn close(self: Box<Self>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn teardown_closes_owned_resource_once_and_never_stops_external() {
        let closes = Arc::new(AtomicUsize::new(0));
        drop(BackendLease(Some(Box::new(CountClose(Arc::clone(
            &closes,
        ))))));
        assert_eq!(closes.load(Ordering::SeqCst), 1);
        drop(BackendLease(None));
        assert_eq!(closes.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cancelled_pin_job_never_opens_a_seed_or_repopulates_its_slot() {
        let root = CoincubeDirectory::new(
            std::env::temp_dir().join(format!("coincube-r16-cancelled-pin-{}", std::process::id())),
        );
        let cancelled = Arc::new(AtomicBool::new(true));
        let busy = Arc::new(AtomicBool::new(true));
        let signer_slot = Arc::new(Mutex::new(None));
        let job = PinRequest {
            root,
            cube: CubeSettings::new_with_raw_id(
                "destination".into(),
                "Destination".into(),
                ChainId::Bitcoin,
            ),
            pin: Zeroizing::new("1234".into()),
            generation: AppGeneration::next(),
            cancelled,
            busy: Arc::clone(&busy),
            signer_slot: Arc::clone(&signer_slot),
        };
        let result = job.run();
        assert!(matches!(result.verdict, Err(Refusal::Stale)));
        assert!(signer_slot.lock().unwrap().is_none());
        assert!(!busy.load(Ordering::Acquire));
    }

    #[test]
    fn duress_outlives_a_stale_or_locked_source_and_tears_down_the_owner() {
        let original = source();
        let mut changed = original.clone();
        changed.app = AppGeneration::next();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut owner = ScopedDestination {
            root: CoincubeDirectory::new(std::env::temp_dir().join("unused-r16-duress")),
            source: original,
            cube: CubeSettings::new_with_raw_id(
                "destination".into(),
                "Destination".into(),
                ChainId::Bitcoin,
            ),
            wallet_id: WalletId::new("destination".into(), Some(1)),
            generation: AppGeneration::next(),
            cancelled: Arc::clone(&cancelled),
            pin_busy: Arc::new(AtomicBool::new(false)),
            pin_signer: Arc::new(Mutex::new(None)),
            verified: false,
            #[cfg(target_os = "macos")]
            ceremony: None,
            backend: None,
        };
        let result = PinCheck {
            generation: AppGeneration::next(),
            verdict: Ok(PinVerdict::Duress),
        };
        assert_eq!(owner.accept_pin(&changed, result), Err(Refusal::Duress));
        assert!(cancelled.load(Ordering::Acquire));
        assert!(!owner.verified);
    }
}
