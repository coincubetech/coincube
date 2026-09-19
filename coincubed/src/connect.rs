//! Ephemeral authenticated Connect authority. Never serialized into daemon configuration.
use crate::config::EsploraConfig;
use coincube_core::chain::ChainId;
use miniscript::bitcoin::BlockHash;
use std::{
    fmt,
    sync::Arc,
    time::{Duration, SystemTime},
};

/// Evidence supplied only from Connect's authenticated, fork-verified anchor contract.
/// The successful contract verifies the actual version-2 post-fork RPC header;
/// this is a trusted-service boundary, not SPV verification of a malicious provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedChainAnchor {
    pub chain: ChainId,
    pub height: u32,
    pub hash: BlockHash,
    pub median_time_past: u32,
    pub observed_at: SystemTime,
}

/// Implementations must bind their authenticated client to the requested chain.
/// They may cache successful responses for at most MAX_ANCHOR_AGE, never errors.
/// Calls occur at operation boundaries, not per transaction within a BDK scan.
pub trait ConnectAnchorAuthority: Send + Sync {
    fn fresh_anchor(&self) -> Result<TrustedChainAnchor, AdmissionError>;
}

pub const MAX_ANCHOR_AGE: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionError {
    MissingAuth,
    Unavailable,
    WrongChain,
    Stale,
    HashMismatch,
    ChangedDuringOperation,
    InvalidBackend,
}
impl fmt::Display for AdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Connect chain admission refused: {:?}", self)
    }
}
impl std::error::Error for AdmissionError {}

/// One immutable provider and authority, with in-memory authentication only.
/// Replacing credentials/provider requires constructing and admitting a new context.
pub struct ConnectBackend {
    chain: ChainId,
    endpoint: String,
    bearer_token: String,
    authority: Arc<dyn ConnectAnchorAuthority>,
}
impl fmt::Debug for ConnectBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectBackend")
            .field("chain", &self.chain)
            .finish_non_exhaustive()
    }
}
impl ConnectBackend {
    pub fn new(
        chain: ChainId,
        endpoint: String,
        bearer_token: String,
        authority: Arc<dyn ConnectAnchorAuthority>,
    ) -> Result<Self, AdmissionError> {
        if !chain.is_blake2b() {
            return Err(AdmissionError::WrongChain);
        }
        if bearer_token.trim().is_empty() {
            return Err(AdmissionError::MissingAuth);
        }
        if endpoint.trim().is_empty() {
            return Err(AdmissionError::InvalidBackend);
        }
        Ok(Self {
            chain,
            endpoint,
            bearer_token,
            authority,
        })
    }
    pub(crate) fn config(&self) -> EsploraConfig {
        EsploraConfig {
            addr: self.endpoint.clone(),
            token: Some(self.bearer_token.clone()),
            fallback_addr: None,
            fallback_token: None,
            secondary_fallback_addr: None,
            secondary_fallback_token: None,
        }
    }
    pub(crate) fn matches_selection(&self, config: &EsploraConfig) -> bool {
        config.addr == self.endpoint
            && config.fallback_addr.is_none()
            && config.secondary_fallback_addr.is_none()
    }
    pub(crate) fn chain(&self) -> ChainId {
        self.chain
    }
    pub(crate) fn validate(
        &self,
        mut hash_at: impl FnMut(u32) -> Result<BlockHash, AdmissionError>,
    ) -> Result<TrustedChainAnchor, AdmissionError> {
        let anchor = self.authority.fresh_anchor()?;
        if anchor.chain != self.chain {
            return Err(AdmissionError::WrongChain);
        }
        let age = SystemTime::now()
            .duration_since(anchor.observed_at)
            .map_err(|_| AdmissionError::Stale)?;
        if age > MAX_ANCHOR_AGE {
            return Err(AdmissionError::Stale);
        }
        if hash_at(anchor.height)? != anchor.hash {
            return Err(AdmissionError::HashMismatch);
        }
        Ok(anchor)
    }
    pub(crate) fn revalidate(
        &self,
        before: &TrustedChainAnchor,
        hash_at: impl FnMut(u32) -> Result<BlockHash, AdmissionError>,
    ) -> Result<(), AdmissionError> {
        let after = self.validate(hash_at)?;
        // A changing trusted tip may simply be new work; retrying is conservative
        // and avoids releasing a scan spanning an unverified reorganization.
        if before.chain != after.chain || before.height != after.height || before.hash != after.hash
        {
            return Err(AdmissionError::ChangedDuringOperation);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniscript::bitcoin::hashes::Hash;
    use std::sync::Mutex;
    struct Authority(Mutex<TrustedChainAnchor>);
    impl ConnectAnchorAuthority for Authority {
        fn fresh_anchor(&self) -> Result<TrustedChainAnchor, AdmissionError> {
            Ok(self.0.lock().unwrap().clone())
        }
    }
    fn fixture() -> (ConnectBackend, Arc<Authority>, BlockHash) {
        let hash = BlockHash::from_byte_array([7; 32]);
        let source = Arc::new(Authority(Mutex::new(TrustedChainAnchor {
            chain: ChainId::BitcoinBlake2b,
            height: 900_000,
            hash,
            median_time_past: 1_000,
            observed_at: SystemTime::now(),
        })));
        let backend = ConnectBackend::new(
            ChainId::BitcoinBlake2b,
            "https://fixture.invalid".into(),
            "synthetic-jwt".into(),
            source.clone(),
        )
        .unwrap();
        (backend, source, hash)
    }
    #[test]
    fn admission_refuses_wrong_chain_stale_future_lag_and_hash() {
        let (backend, source, hash) = fixture();
        assert!(backend.validate(|_| Ok(hash)).is_ok());
        assert_eq!(
            backend.validate(|_| Err(AdmissionError::Unavailable)),
            Err(AdmissionError::Unavailable)
        );
        assert_eq!(
            backend.validate(|_| Ok(BlockHash::from_byte_array([8; 32]))),
            Err(AdmissionError::HashMismatch)
        );
        source.0.lock().unwrap().chain = ChainId::Bitcoin;
        assert_eq!(
            backend.validate(|_| panic!("wrong chain must not call provider")),
            Err(AdmissionError::WrongChain)
        );
        source.0.lock().unwrap().chain = ChainId::BitcoinBlake2b;
        source.0.lock().unwrap().observed_at =
            SystemTime::now() - MAX_ANCHOR_AGE - Duration::from_secs(1);
        assert_eq!(
            backend.validate(|_| panic!("stale authority must not call provider")),
            Err(AdmissionError::Stale)
        );
        source.0.lock().unwrap().observed_at = SystemTime::now() + Duration::from_secs(60);
        assert_eq!(backend.validate(|_| Ok(hash)), Err(AdmissionError::Stale));
    }
    #[test]
    fn changed_anchor_discards_result_and_never_reuses_provider_trust() {
        let (backend, source, hash) = fixture();
        let before = backend.validate(|_| Ok(hash)).unwrap();
        source.0.lock().unwrap().hash = BlockHash::from_byte_array([9; 32]);
        assert_eq!(
            backend.revalidate(&before, |_| Ok(hash)),
            Err(AdmissionError::HashMismatch)
        );
        let new_hash = source.0.lock().unwrap().hash;
        assert_eq!(
            backend.revalidate(&before, |_| Ok(new_hash)),
            Err(AdmissionError::ChangedDuringOperation)
        );
        let current = backend.validate(|_| Ok(new_hash)).unwrap();
        assert!(backend.revalidate(&current, |_| Ok(new_hash)).is_ok());
    }
    #[test]
    fn runtime_auth_is_required_and_redacted() {
        let (backend, source, _) = fixture();
        assert!(!format!("{:?}", backend).contains("synthetic-jwt"));
        assert_eq!(
            ConnectBackend::new(
                ChainId::BitcoinBlake2b,
                "fixture".into(),
                String::new(),
                source
            )
            .unwrap_err(),
            AdmissionError::MissingAuth
        );
        let config = backend.config();
        assert!(config.fallback_addr.is_none() && config.secondary_fallback_addr.is_none());
    }
}
