//! Opt-in transport for headless Claim tests against disposable regtest nodes.
//! This does not admit regtest into production Claim or start a wallet daemon.
//! The harness translates logical Bitcoin/BTCB2 roles explicitly while retaining
//! exact-byte gate consumption, no retry, and real RPC submission outcomes.
use super::*;
use miniscript::bitcoin::{
    base64::{engine::general_purpose::STANDARD, Engine},
    consensus::encode::serialize_hex,
};
use serde_json::{json, Value};
use std::{io::Read, net::SocketAddr, sync::Mutex};

pub struct RegtestTransport {
    endpoint: SocketAddr,
    authorization: String,
    chain: ChainId,
    descriptor: CoincubeDescriptor,
    serial: Mutex<()>,
}
impl RegtestTransport {
    /// Credentials must belong to the harness's disposable node. Only literal
    /// loopback addresses are accepted; redirects are never followed.
    pub fn new(
        endpoint: SocketAddr,
        cookie: &str,
        chain: ChainId,
        descriptor: CoincubeDescriptor,
    ) -> Result<Self, SubmissionError> {
        if !endpoint.ip().is_loopback()
            || !matches!(chain, ChainId::Bitcoin | ChainId::BitcoinBlake2b)
        {
            return Err(SubmissionError::UnsupportedChain);
        }
        let transport = Self {
            endpoint,
            authorization: format!("Basic {}", STANDARD.encode(cookie)),
            chain,
            descriptor,
            serial: Mutex::new(()),
        };
        transport.check_regtest()?;
        Ok(transport)
    }
    fn rpc(&self, method: &str, params: Value) -> Result<Value, SubmissionError> {
        let response = minreq::post(format!("http://{}", self.endpoint))
            .with_max_redirects(0)
            .with_max_headers_size(16 * 1024)
            .with_max_status_line_length(1024)
            .with_timeout(5)
            .with_header("Authorization", &self.authorization)
            .with_header("Content-Type", "application/json")
            .with_body(
                json!({"jsonrpc":"2.0","id":"claim-regtest","method":method,"params":params})
                    .to_string(),
            )
            .send_lazy()
            .map_err(|_| SubmissionError::BackendUnavailable)?;
        if response.status_code != 200 {
            return Err(SubmissionError::BackendUnavailable);
        }
        let mut body = Vec::new();
        Read::take(response, 1024 * 1024 + 1)
            .read_to_end(&mut body)
            .map_err(|_| SubmissionError::BackendUnavailable)?;
        if body.len() > 1024 * 1024 {
            return Err(SubmissionError::BackendUnavailable);
        }
        let value: Value =
            serde_json::from_slice(&body).map_err(|_| SubmissionError::BackendUnavailable)?;
        if value.get("id") != Some(&json!("claim-regtest"))
            || value.get("error").is_some_and(|error| !error.is_null())
        {
            return Err(SubmissionError::BackendUnavailable);
        }
        value
            .get("result")
            .cloned()
            .ok_or(SubmissionError::BackendUnavailable)
    }
    fn check_regtest(&self) -> Result<(), SubmissionError> {
        if self
            .rpc("getblockchaininfo", json!([]))?
            .get("chain")
            .and_then(Value::as_str)
            != Some("regtest")
        {
            return Err(SubmissionError::UnsupportedChain);
        }
        Ok(())
    }
    pub fn submit_poison(
        &self,
        verified: &VerifiedPoisonTransfer,
        gate: &SubmissionGate,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        if verified.chain() != ChainId::Bitcoin {
            return Err(SubmissionError::UnsupportedChain);
        }
        self.submit(
            verified.chain(),
            verified.descriptor(),
            verified.transaction(),
            gate,
        )
    }
    pub fn submit_fork(
        &self,
        verified: &VerifiedClaimForkSweep,
        gate: &SubmissionGate,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        if verified.chain() != ChainId::BitcoinBlake2b {
            return Err(SubmissionError::UnsupportedChain);
        }
        self.submit(
            verified.chain(),
            verified.descriptor(),
            verified.transaction(),
            gate,
        )
    }
    fn submit(
        &self,
        chain: ChainId,
        descriptor: &CoincubeDescriptor,
        transaction: &Transaction,
        gate: &SubmissionGate,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        if chain != self.chain {
            return Err(SubmissionError::UnsupportedChain);
        }
        if descriptor != &self.descriptor {
            return Err(SubmissionError::DescriptorMismatch);
        }
        let txid = transaction.compute_txid();
        let wtxid = transaction.compute_wtxid();
        if gate.chain != chain || gate.txid != txid || gate.wtxid != wtxid {
            return Err(SubmissionError::GateMismatch);
        }
        let _serial = self
            .serial
            .lock()
            .map_err(|_| SubmissionError::BackendUnavailable)?;
        // Refuse a replacement node running another network before consuming
        // the gate. The final atomic gate transition follows this read.
        self.check_regtest()?;
        gate.enter()?;
        let result = self.rpc("sendrawtransaction", json!([serialize_hex(transaction)]));
        if result.ok().as_ref().and_then(Value::as_str) != Some(txid.to_string().as_str()) {
            return Err(SubmissionError::Uncertain { txid, wtxid });
        }
        Ok(SubmissionOutcome::UpstreamAccepted { txid, wtxid })
    }
}
