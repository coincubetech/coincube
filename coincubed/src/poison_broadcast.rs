//! Exact-byte transport only. This module grants no Claim/broadcast authorization.
use crate::DaemonControl;
use coincube_core::{chain::ChainId, claim_finalize::VerifiedPoisonTransfer};
use miniscript::bitcoin::{Txid, Wtxid};
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
            Self::UnsupportedChain => f.write_str("Poison submission requires Bitcoin mainnet"),
            Self::BackendUnavailable => {
                f.write_str("Bitcoin backend is unavailable before submission")
            }
            Self::GateMismatch => f.write_str("Submission gate belongs to another transaction"),
            Self::Revoked => f.write_str("Submission was revoked before transport started"),
            Self::AlreadyStarted => f.write_str("Submission gate was already consumed"),
            Self::DescriptorMismatch => {
                f.write_str("Poison construction belongs to another descriptor")
            }
            Self::Uncertain { .. } => f.write_str(
                "Poison submission may have been accepted; reconcile the exact transaction",
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
}
fn state(value: u8) -> SubmissionState {
    match value {
        0 => SubmissionState::Pending,
        1 => SubmissionState::Revoked,
        _ => SubmissionState::Started,
    }
}
/// One-use, transaction-bound transport gate. No reset or deserialization.
/// The coordinator owns context checks, user approval and durable intent.
pub struct SubmissionGate {
    state: Arc<AtomicU8>,
    chain: ChainId,
    txid: Txid,
    wtxid: Wtxid,
    #[cfg(test)]
    before_lock: Option<Arc<std::sync::Barrier>>,
}
/// Cloneable cancellation handle; revocation after Started cannot retract bytes.
#[derive(Clone)]
pub struct SubmissionRevoker {
    state: Arc<AtomicU8>,
}
impl SubmissionGate {
    pub fn new(verified: &VerifiedPoisonTransfer) -> (Self, SubmissionRevoker) {
        let state = Arc::new(AtomicU8::new(0));
        (
            Self {
                state: state.clone(),
                chain: verified.chain(),
                txid: verified.transaction().compute_txid(),
                wtxid: verified.transaction().compute_wtxid(),
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
        self.state
            .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| ())
            .map_err(|value| {
                if value == 1 {
                    SubmissionError::Revoked
                } else {
                    SubmissionError::AlreadyStarted
                }
            })
    }
}
impl SubmissionRevoker {
    /// Returns Revoked when cancellation won, or Started when transport may run.
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
        if &self.config.main_descriptor != verified.descriptor() {
            return Err(SubmissionError::DescriptorMismatch);
        }
        let transaction = verified.transaction();
        let txid = transaction.compute_txid();
        let wtxid = transaction.compute_wtxid();
        if gate.chain != verified.chain() || gate.txid != txid || gate.wtxid != wtxid {
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
