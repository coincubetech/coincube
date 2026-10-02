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
    foreign_split::{VerifiedSplitStep1, VerifiedSplitStep2},
};
use miniscript::bitcoin::{bip32::ChildNumber, Transaction, Txid, Wtxid};
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
    /// Split (#568) step 2: the transaction does not pay exactly one output,
    /// the receive address of this daemon's own Vault at the recorded index.
    OutputMismatch,
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
            Self::OutputMismatch => {
                f.write_str("Split step 2 does not pay this Vault's reserved address")
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
    /// Split (#568) step-1 transport gate only. Fresh two-chain evidence,
    /// explicit approval and the durable Split submission intent remain the
    /// coordinator's responsibility; verified signatures are not permission.
    pub fn for_split_step1(
        verified: &VerifiedSplitStep1,
        not_after: std::time::Instant,
    ) -> (Self, SubmissionRevoker) {
        Self::for_transaction(verified.chain(), verified.transaction(), not_after)
    }
    /// Split (#568) step-2 transport gate only. The coordinator owns the
    /// fresh six-confirmation check, the BTCB2 preflight of this witness,
    /// explicit approval and the durable step-2 submission intent.
    pub fn for_split_step2(
        verified: &VerifiedSplitStep2,
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

/// Daemonless Split (#568) step-1 transport: submit the exact verified bytes
/// once to the fixed Bitcoin mainnet Connect endpoint at `origin`. A Split has
/// no Bitcoin Cube and so no daemon, descriptor or backend binding (D5); this
/// is the only route. Blocking: async callers run it on a blocking worker.
///
/// The caller must first join fresh chain observations and operator
/// preflight of this exact witness at this origin, obtain explicit user
/// approval and durably record the Split submission intent. The artifact and
/// gate grant nothing. No daemon RPC or `DaemonControl` method exposes this,
/// and there is no retry, redirect, proxy or fallback: a transport failure
/// after the gate was entered is `Uncertain` and must only be reconciled.
pub fn submit_verified_split_step1_to_connect(
    verified: &VerifiedSplitStep1,
    gate: &SubmissionGate,
    origin: &str,
) -> Result<SubmissionOutcome, SubmissionError> {
    // Bitcoin mainnet only: the Connect route is fixed to it, and Testnet4
    // stays refused until a route for it is verified.
    if verified.chain() != ChainId::Bitcoin {
        return Err(SubmissionError::UnsupportedChain);
    }
    let transaction = verified.transaction();
    let txid = transaction.compute_txid();
    let wtxid = transaction.compute_wtxid();
    if gate.chain != ChainId::Bitcoin || gate.txid != txid || gate.wtxid != wtxid {
        return Err(SubmissionError::GateMismatch);
    }
    let request = connect_transport::PreparedConnect::new(origin, transaction)
        .map_err(|_| SubmissionError::BackendUnavailable)?;
    // With no backend lock to hold the worker, a test acts between the two
    // rendezvous: after preparation, before the gate is entered.
    #[cfg(test)]
    if let Some(barrier) = &gate.before_lock {
        barrier.wait();
        barrier.wait();
    }
    // Linearization point: revocation or expiry wins before this atomic
    // transition; afterwards the one attempt has started. Nothing between it
    // and the request can fail or wait on another lock.
    gate.enter()?;
    request
        .send()
        .map_err(|_| SubmissionError::Uncertain { txid, wtxid })?;
    Ok(SubmissionOutcome::UpstreamAccepted { txid, wtxid })
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
    ///
    /// This route holds no signing PSBT, only finalised bytes, so it cannot
    /// run the unsafe-legacy-alternative refusal itself (`#582`). The refusal
    /// runs in `finalize_claim_fork_sweep`, the only constructor of the
    /// artifact that reads a PSBT; the other, `verify_claim_fork_transaction`,
    /// rebuilds an already-published witness that the coordinator will not
    /// record for submission again.
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

    /// Split (#568 B3b) step 2 through this daemon's own backend: the BTCB2
    /// Vault daemon admitted by the coordinator (the exact Connect BTCB2
    /// Esplora route). Not exposed over RPC; one attempt, no retry.
    ///
    /// The inputs are a foreign wallet's, so there is no descriptor check on
    /// them: `finalize_split_step2` verified every signature against the
    /// recorded foreign source. What this route does check is where the coins
    /// go: exactly one output, paying this daemon's own Vault receive address
    /// at `target_index` (the index recorded in the Split journal), on BTCB2
    /// mainnet only. The caller must first join the fresh step-2 checks, the
    /// BTCB2 preflight of this witness, approval and the durable intent.
    pub fn submit_verified_split_step2(
        &self,
        verified: &VerifiedSplitStep2,
        target_index: ChildNumber,
        gate: &SubmissionGate,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        let transaction = self.check_split_step2(verified, target_index, gate)?;
        let txid = transaction.compute_txid();
        let wtxid = transaction.compute_wtxid();
        #[cfg(test)]
        if let Some(barrier) = &gate.before_lock {
            barrier.wait();
        }
        let backend = self
            .bitcoin
            .lock()
            .map_err(|_| SubmissionError::BackendUnavailable)?;
        gate.enter()?;
        backend
            .broadcast_tx(transaction)
            .map_err(|_| SubmissionError::Uncertain { txid, wtxid })?;
        Ok(SubmissionOutcome::UpstreamAccepted { txid, wtxid })
    }

    /// Split (#568 B3b, owner decision P4) step 2 to the bound node of a
    /// BTCB2 Vault daemon on a managed Knots node: the same checks as
    /// [`Self::submit_verified_split_step2`], plus the binding the
    /// coordinator captured when it preflighted this witness through that
    /// node. One `sendrawtransaction`, no redirect, retry or fallback.
    pub fn submit_verified_split_step2_to_node(
        &self,
        verified: &VerifiedSplitStep2,
        target_index: ChildNumber,
        binding: &ClaimBackendBinding,
        gate: &SubmissionGate,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        let transaction = self.check_split_step2(verified, target_index, gate)?;
        let txid = transaction.compute_txid();
        let wtxid = transaction.compute_wtxid();
        if !binding.matches(self) {
            return Err(SubmissionError::BackendUnavailable);
        }
        let Some(crate::config::BitcoinBackend::Bitcoind(node)) = &self.config.bitcoin_backend
        else {
            return Err(SubmissionError::BackendUnavailable);
        };
        let request = node_transport::PreparedNode::new(node, transaction)
            .map_err(|_| SubmissionError::BackendUnavailable)?;
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
        request
            .send()
            .map_err(|_| SubmissionError::Uncertain { txid, wtxid })?;
        Ok(SubmissionOutcome::UpstreamAccepted { txid, wtxid })
    }

    /// Step 2's checks before any lock: BTCB2 mainnet on both sides, one
    /// output equal to this Vault's receive derivation at `target_index`,
    /// and a gate for exactly this witness.
    fn check_split_step2<'a>(
        &self,
        verified: &'a VerifiedSplitStep2,
        target_index: ChildNumber,
        gate: &SubmissionGate,
    ) -> Result<&'a Transaction, SubmissionError> {
        if self.config.bitcoin_config.chain != ChainId::BitcoinBlake2b
            || verified.chain() != ChainId::BitcoinBlake2b
            || self.config.bitcoin_config.network != miniscript::bitcoin::Network::Bitcoin
        {
            return Err(SubmissionError::UnsupportedChain);
        }
        let transaction = verified.transaction();
        if target_index.is_hardened()
            || transaction.output.len() != 1
            || transaction.output[0].script_pubkey
                != self
                    .config
                    .main_descriptor
                    .receive_descriptor()
                    .derive(target_index, &self.secp)
                    .script_pubkey()
        {
            return Err(SubmissionError::OutputMismatch);
        }
        if gate.chain != ChainId::BitcoinBlake2b
            || gate.txid != transaction.compute_txid()
            || gate.wtxid != transaction.compute_wtxid()
        {
            return Err(SubmissionError::GateMismatch);
        }
        Ok(transaction)
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
mod split_step2_tests;
#[cfg(test)]
mod split_tests;
#[cfg(test)]
mod tests;
