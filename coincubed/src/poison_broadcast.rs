//! Exact-byte transport only. This module grants no Claim/broadcast authorization.
use crate::{bitcoin::BitcoinInterface, DaemonControl};
use coincube_core::{chain::ChainId, claim_finalize::VerifiedPoisonTransfer};
use miniscript::bitcoin::{Txid, Wtxid};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionError {
    UnsupportedChain,
    DescriptorMismatch,
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
        self.bitcoin
            .broadcast_tx(transaction)
            .map_err(|_| SubmissionError::Uncertain { txid, wtxid })?;
        Ok(SubmissionOutcome::UpstreamAccepted { txid, wtxid })
    }
}

#[cfg(test)]
mod tests;
