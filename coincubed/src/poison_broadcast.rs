//! Exact-byte transport only. This module grants no Claim/broadcast authorization.
mod binding;
mod connect_transport;
mod node_transport;
#[cfg(feature = "regtest-harness")]
pub mod regtest_harness;
use crate::DaemonControl;
pub use binding::ClaimBackendBinding;
use coincube_core::{
    chain::ChainId,
    claim_finalize::{VerifiedAncestryTransfer, VerifiedClaimForkSweep, VerifiedPoisonTransfer},
    descriptors::CoincubeDescriptor,
};
use miniscript::bitcoin::{Transaction, Txid, Wtxid};
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionError {
    UnsupportedChain,
    DescriptorMismatch,
    BackendUnavailable,
    GateMismatch,
    Revoked,
    Expired,
    AlreadyStarted,
    /// The backend may have accepted the transaction before returning an error.
    /// Reconcile this exact txid/wtxid; never silently retry or replace it.
    Uncertain {
        txid: Txid,
        wtxid: Wtxid,
    },
}
impl std::fmt::Display for SubmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedChain => {
                f.write_str("Claim submission chain is unsupported or mismatched")
            }
            Self::BackendUnavailable => {
                f.write_str("Chain backend is unavailable before submission")
            }
            Self::GateMismatch => f.write_str("Submission gate belongs to another transaction"),
            Self::Expired => f.write_str("Submission evidence expired before transport started"),
            Self::Revoked => f.write_str("Submission was revoked before transport started"),
            Self::AlreadyStarted => f.write_str("Submission gate was already consumed"),
            Self::DescriptorMismatch => {
                f.write_str("Claim construction belongs to another descriptor")
            }
            Self::Uncertain { .. } => f.write_str(
                "Claim submission may have been accepted; reconcile the exact transaction",
            ),
        }
    }
}
impl std::error::Error for SubmissionError {}

/// Upstream acknowledgement only, not confirmation, exclusivity or Claim permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionOutcome {
    UpstreamAccepted { txid: Txid, wtxid: Wtxid },
}

/// Transport scheduling state only, never authorization or confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionState {
    Pending,
    Revoked,
    Started,
    Expired,
}
fn state(value: u8) -> SubmissionState {
    match value {
        0 => SubmissionState::Pending,
        1 => SubmissionState::Revoked,
        2 => SubmissionState::Started,
        _ => SubmissionState::Expired,
    }
}
/// One-use, transaction-bound transport gate. No reset or deserialization.
/// The coordinator owns context checks, user approval and durable intent.
pub struct SubmissionGate {
    state: Arc<AtomicU8>,
    chain: ChainId,
    txid: Txid,
    wtxid: Wtxid,
    not_after: std::time::Instant,
    #[cfg(test)]
    before_lock: Option<Arc<std::sync::Barrier>>,
}
/// Cloneable cancellation handle; revocation after Started cannot retract bytes.
#[derive(Clone)]
pub struct SubmissionRevoker {
    state: Arc<AtomicU8>,
}
impl SubmissionGate {
    /// Required caller deadline; no default and no transport-defined freshness.
    pub fn new(
        verified: &VerifiedPoisonTransfer,
        not_after: std::time::Instant,
    ) -> (Self, SubmissionRevoker) {
        Self::for_transaction(verified.chain(), verified.transaction(), not_after)
    }
    /// Ancestry transport only. Fresh chain qualification and durable consent
    /// remain the coordinator's responsibility; signatures are not authorization.
    pub fn for_ancestry(
        verified: &VerifiedAncestryTransfer,
        not_after: std::time::Instant,
    ) -> (Self, SubmissionRevoker) {
        Self::for_transaction(verified.chain(), verified.transaction(), not_after)
    }
    /// Fork transport gate only. Fresh split evidence, approval and durable
    /// submission intent remain the coordinator's responsibility.
    pub fn for_claim_fork(
        verified: &VerifiedClaimForkSweep,
        not_after: std::time::Instant,
    ) -> (Self, SubmissionRevoker) {
        Self::for_transaction(verified.chain(), verified.transaction(), not_after)
    }
    fn for_transaction(
        chain: ChainId,
        transaction: &Transaction,
        not_after: std::time::Instant,
    ) -> (Self, SubmissionRevoker) {
        let state = Arc::new(AtomicU8::new(0));
        (
            Self {
                state: state.clone(),
                chain,
                txid: transaction.compute_txid(),
                wtxid: transaction.compute_wtxid(),
                not_after,
                #[cfg(test)]
                before_lock: None,
            },
            SubmissionRevoker { state },
        )
    }
    pub fn state(&self) -> SubmissionState {
        state(self.state.load(Ordering::SeqCst))
    }
    fn enter(&self) -> Result<(), SubmissionError> {
        // The caller supplies a conservative monotonic lifetime from its
        // evidence. Queue time consumes it; the transport invents no duration.
        let expired = std::time::Instant::now() >= self.not_after;
        let next = if expired { 3 } else { 2 };
        match self
            .state
            .compare_exchange(0, next, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) if expired => Err(SubmissionError::Expired),
            Ok(_) => Ok(()),
            Err(1) => Err(SubmissionError::Revoked),
            Err(3) => Err(SubmissionError::Expired),
            Err(_) => Err(SubmissionError::AlreadyStarted),
        }
    }
}
impl SubmissionRevoker {
    /// Returns Revoked when cancellation won, Started when transport may run,
    /// or Expired when a previous entry attempt already refused the deadline.
    pub fn revoke(&self) -> SubmissionState {
        match self
            .state
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => SubmissionState::Revoked,
            Err(value) => state(value),
        }
    }
    pub fn state(&self) -> SubmissionState {
        state(self.state.load(Ordering::SeqCst))
    }
}

impl DaemonControl {
    /// Dormant embedded transport. Caller must first join fresh RDTS/chain and
    /// UTXO observations, exact-witness preflight, explicit user approval and
    /// durable uncertain-intent journaling. The cryptographic artifact alone is
    /// NOT authorization. No RPC method exposes this opaque argument.
    ///
    /// Forward exactly the verified bytes, without loading a stored PSBT or
    /// finalizing again. Testnet4 remains unsupported until its route is verified.
    /// No retries, persistence or poller wait can blur the submission outcome.
    pub fn submit_verified_poison(
        &self,
        verified: &VerifiedPoisonTransfer,
        gate: &SubmissionGate,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        if self.config.bitcoin_config.chain != ChainId::Bitcoin
            || verified.chain() != ChainId::Bitcoin
        {
            return Err(SubmissionError::UnsupportedChain);
        }
        self.submit_exact_claim_transaction(
            verified.chain(),
            verified.descriptor(),
            verified.transaction(),
            gate,
        )
    }

    /// Exact-byte ancestry transport, with no RPC exposure or automatic retry.
    /// The caller must first qualify ancestry against fresh chain observations,
    /// preflight this witness, obtain consent and persist uncertain intent.
    pub fn submit_verified_ancestry(
        &self,
        verified: &VerifiedAncestryTransfer,
        gate: &SubmissionGate,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        if self.config.bitcoin_config.chain != ChainId::Bitcoin
            || verified.chain() != ChainId::Bitcoin
        {
            return Err(SubmissionError::UnsupportedChain);
        }
        self.submit_exact_claim_transaction(
            verified.chain(),
            verified.descriptor(),
            verified.transaction(),
            gate,
        )
    }

    /// Dormant embedded fork transport; no RPC exposes this opaque artifact.
    /// The caller must first verify fresh Bitcoin poison confirmation, fork
    /// inputs and tips, obtain approval and persist uncertain submission intent.
    /// Verified signatures alone do not prove replay safety or grant permission.
    /// Like step one, testnet transport remains disabled pending route testing.
    pub fn submit_verified_claim_fork(
        &self,
        verified: &VerifiedClaimForkSweep,
        gate: &SubmissionGate,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        if self.config.bitcoin_config.chain != ChainId::BitcoinBlake2b
            || verified.chain() != ChainId::BitcoinBlake2b
        {
            return Err(SubmissionError::UnsupportedChain);
        }
        self.submit_exact_claim_transaction(
            verified.chain(),
            verified.descriptor(),
            verified.transaction(),
            gate,
        )
    }

    /// Submit the exact verified Bitcoin witness to the bound configured node.
    /// The coordinator must preflight this route, obtain consent and journal
    /// uncertain intent first. This method is not exposed over daemon RPC.
    pub fn submit_verified_poison_to_node(
        &self,
        verified: &VerifiedPoisonTransfer,
        binding: &ClaimBackendBinding,
        gate: &SubmissionGate,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        self.submit_exact_bound_claim(
            verified.chain(),
            verified.descriptor(),
            verified.transaction(),
            binding,
            gate,
            None,
        )
    }

    /// Same bound, single-attempt route for a verified ancestry transaction.
    pub fn submit_verified_ancestry_to_node(
        &self,
        verified: &VerifiedAncestryTransfer,
        binding: &ClaimBackendBinding,
        gate: &SubmissionGate,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        self.submit_exact_bound_claim(
            verified.chain(),
            verified.descriptor(),
            verified.transaction(),
            binding,
            gate,
            None,
        )
    }

    /// Submit once to the fixed Bitcoin Connect endpoint. The caller must bind
    /// this origin to fresh operator preflight, user consent and durable intent.
    pub fn submit_verified_poison_to_connect(
        &self,
        verified: &VerifiedPoisonTransfer,
        binding: &ClaimBackendBinding,
        gate: &SubmissionGate,
        origin: &str,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        self.submit_exact_bound_claim(
            verified.chain(),
            verified.descriptor(),
            verified.transaction(),
            binding,
            gate,
            Some(origin),
        )
    }

    /// Submit once to the fixed Bitcoin Connect endpoint. The caller must bind
    /// this origin to fresh operator preflight, user consent and durable intent.
    pub fn submit_verified_ancestry_to_connect(
        &self,
        verified: &VerifiedAncestryTransfer,
        binding: &ClaimBackendBinding,
        gate: &SubmissionGate,
        origin: &str,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        self.submit_exact_bound_claim(
            verified.chain(),
            verified.descriptor(),
            verified.transaction(),
            binding,
            gate,
            Some(origin),
        )
    }

    fn submit_exact_bound_claim(
        &self,
        chain: ChainId,
        descriptor: &CoincubeDescriptor,
        transaction: &Transaction,
        binding: &ClaimBackendBinding,
        gate: &SubmissionGate,
        connect_origin: Option<&str>,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        if chain != ChainId::Bitcoin || self.config.bitcoin_config.chain != chain {
            return Err(SubmissionError::UnsupportedChain);
        }
        if &self.config.main_descriptor != descriptor {
            return Err(SubmissionError::DescriptorMismatch);
        }
        let txid = transaction.compute_txid();
        let wtxid = transaction.compute_wtxid();
        if gate.chain != chain || gate.txid != txid || gate.wtxid != wtxid {
            return Err(SubmissionError::GateMismatch);
        }
        if !binding.matches(self) {
            return Err(SubmissionError::BackendUnavailable);
        }
        enum Prepared {
            Node(node_transport::PreparedNode),
            Connect(connect_transport::PreparedConnect),
        }
        let prepared = match connect_origin {
            Some(origin) => Prepared::Connect(
                connect_transport::PreparedConnect::new(origin, transaction)
                    .map_err(|_| SubmissionError::BackendUnavailable)?,
            ),
            None => {
                let Some(crate::config::BitcoinBackend::Bitcoind(node)) =
                    &self.config.bitcoin_backend
                else {
                    return Err(SubmissionError::BackendUnavailable);
                };
                Prepared::Node(
                    node_transport::PreparedNode::new(node, transaction)
                        .map_err(|_| SubmissionError::BackendUnavailable)?,
                )
            }
        };
        #[cfg(test)]
        if let Some(barrier) = &gate.before_lock {
            barrier.wait();
        }
        let _backend = self
            .bitcoin
            .lock()
            .map_err(|_| SubmissionError::BackendUnavailable)?;
        if !binding.matches(self) {
            return Err(SubmissionError::BackendUnavailable);
        }
        gate.enter()?;
        match prepared {
            Prepared::Node(request) => request.send(),
            Prepared::Connect(request) => request.send(),
        }
        .map_err(|_| SubmissionError::Uncertain { txid, wtxid })?;
        Ok(SubmissionOutcome::UpstreamAccepted { txid, wtxid })
    }

    fn submit_exact_claim_transaction(
        &self,
        chain: ChainId,
        descriptor: &CoincubeDescriptor,
        transaction: &Transaction,
        gate: &SubmissionGate,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        if &self.config.main_descriptor != descriptor {
            return Err(SubmissionError::DescriptorMismatch);
        }
        let txid = transaction.compute_txid();
        let wtxid = transaction.compute_wtxid();
        if gate.chain != chain || gate.txid != txid || gate.wtxid != wtxid {
            return Err(SubmissionError::GateMismatch);
        }
        #[cfg(test)]
        if let Some(barrier) = &gate.before_lock {
            barrier.wait();
        }
        let backend = self
            .bitcoin
            .lock()
            .map_err(|_| SubmissionError::BackendUnavailable)?;
        // Linearization point: cancellation wins before this atomic transition;
        // afterwards submission has started. No await or second lock before I/O.
        gate.enter()?;
        backend
            .broadcast_tx(transaction)
            .map_err(|_| SubmissionError::Uncertain { txid, wtxid })?;
        Ok(SubmissionOutcome::UpstreamAccepted { txid, wtxid })
    }
}

#[cfg(test)]
mod tests;
