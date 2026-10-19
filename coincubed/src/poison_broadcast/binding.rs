//! In-memory backend binding for Claim review and final submission checks.
use super::SubmissionError;
use crate::{
    bitcoin::BitcoinInterface,
    config::{BitcoinBackend, EsploraConfig},
    DaemonControl,
};
use coincube_core::{chain::ChainId, descriptors::CoincubeDescriptor};
use miniscript::bitcoin::Network;
use std::sync::{Arc, Mutex, Weak};

/// Identity of a daemon backend instance and its immutable routing settings.
/// This is neither a transaction authorization nor a durable node identity.
/// Private credentials are compared in memory; no hash or serialized form is
/// exposed. A weak pointer prevents a saved review from keeping a daemon alive.
#[derive(Clone)]
pub struct ClaimBackendBinding {
    backend: Weak<Mutex<dyn BitcoinInterface>>,
    chain: ChainId,
    network: Network,
    descriptor: Option<CoincubeDescriptor>,
    selection: Option<BitcoinBackend>,
    fallback: Option<EsploraConfig>,
}
impl std::fmt::Debug for ClaimBackendBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimBackendBinding")
            .field("chain", &self.chain)
            .finish_non_exhaustive()
    }
}
impl PartialEq for ClaimBackendBinding {
    fn eq(&self, other: &Self) -> bool {
        self.backend.ptr_eq(&other.backend)
            && self.chain == other.chain
            && self.network == other.network
            && self.descriptor == other.descriptor
            && self.selection == other.selection
            && self.fallback == other.fallback
    }
}
impl ClaimBackendBinding {
    pub(super) fn matches(&self, control: &DaemonControl) -> bool {
        self == &control.claim_backend_binding()
    }

    /// Synthetic identity for a GUI flow mock. Its empty weak backend cannot
    /// pass a real daemon's submission check. Production bindings must always
    /// come from the live daemon controller.
    #[cfg(feature = "test-utils")]
    #[doc(hidden)]
    pub fn for_test(config: &crate::config::Config) -> Self {
        let backend: Weak<Mutex<dyn BitcoinInterface>> =
            Weak::<Mutex<crate::bitcoin::d::BitcoinD>>::new();
        Self {
            backend,
            chain: config.bitcoin_config.chain,
            network: config.bitcoin_config.network,
            descriptor: config.main_descriptor.as_vault().cloned(),
            selection: config.bitcoin_backend.clone(),
            fallback: config.fallback_esplora.clone(),
        }
    }
}
impl DaemonControl {
    /// Capture this controller's backend instance and configuration for review.
    /// Configuration is immutable in a controller; its clones share one backend.
    /// This capture does not lock the backend or grant submission permission.
    pub fn claim_backend_binding(&self) -> ClaimBackendBinding {
        ClaimBackendBinding {
            backend: Arc::downgrade(&self.bitcoin),
            chain: self.config.bitcoin_config.chain,
            network: self.config.bitcoin_config.network,
            descriptor: self.config.main_descriptor.as_vault().cloned(),
            selection: self.config.bitcoin_backend.clone(),
            fallback: self.config.fallback_esplora.clone(),
        }
    }

    /// Read-only binding check under the actual backend lock. Submission must
    /// repeat this check under the same lock held through gate entry and I/O;
    /// success here is not a reusable submission capability.
    pub fn check_claim_backend_binding(
        &self,
        binding: &ClaimBackendBinding,
    ) -> Result<(), SubmissionError> {
        let _backend = self
            .bitcoin
            .lock()
            .map_err(|_| SubmissionError::BackendUnavailable)?;
        if !binding.matches(self) {
            return Err(SubmissionError::BackendUnavailable);
        }
        Ok(())
    }
}
