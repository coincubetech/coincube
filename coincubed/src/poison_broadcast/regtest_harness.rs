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
    node: crate::config::BitcoindConfig,
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
        let (user, password) = cookie
            .split_once(':')
            .ok_or(SubmissionError::BackendUnavailable)?;
        if user.is_empty() || password.is_empty() {
            return Err(SubmissionError::BackendUnavailable);
        }
        let transport = Self {
            endpoint,
            node: crate::config::BitcoindConfig {
                addr: endpoint,
                rpc_auth: crate::config::BitcoindRpcAuth::UserPass(user.into(), password.into()),
            },
            authorization: format!("Basic {}", STANDARD.encode(cookie)),
            chain,
            descriptor,
            serial: Mutex::new(()),
        };
        transport.check_regtest()?;
        Ok(transport)
    }
    /// Build a controller for the production bound-send methods against the
    /// already verified disposable Bitcoin node. The database must be new.
    /// Logical chain identity is explicit: this does not admit regtest wallets
    /// in the product. The idle worker supplies handle cleanup semantics only;
    /// wallet polling/bootstrap is deliberately outside this transport fixture.
    pub fn bound_handle(
        &self,
        directory: std::path::PathBuf,
        electrum: Option<SocketAddr>,
    ) -> Result<(crate::config::Config, crate::DaemonHandle), SubmissionError> {
        use crate::{
            bitcoin::{d::BitcoinD, poller::PollerMessage},
            config::{BitcoinBackend, BitcoinConfig, Config},
            database::sqlite::{FreshDbOptions, SqliteDb},
            datadir::DataDirectory,
        };
        use std::sync::{atomic::AtomicBool, mpsc, Arc};
        if self.chain != ChainId::Bitcoin || electrum.is_some_and(|addr| !addr.ip().is_loopback()) {
            return Err(SubmissionError::UnsupportedChain);
        }
        self.check_regtest()?;
        std::fs::create_dir(&directory).map_err(|_| SubmissionError::BackendUnavailable)?;
        let secp = miniscript::bitcoin::secp256k1::Secp256k1::verification_only();
        let database = SqliteDb::new(
            directory.join("transport.sqlite3"),
            Some(FreshDbOptions::new(self.chain, self.descriptor.clone())),
            &secp,
        )
        .map_err(|_| SubmissionError::BackendUnavailable)?;
        let mut config = Config::new(
            BitcoinConfig::new(self.chain, std::time::Duration::from_secs(2)),
            None,
            log::LevelFilter::Off,
            self.descriptor.clone(),
            DataDirectory::new(directory),
        );
        let database: Arc<Mutex<dyn crate::database::DatabaseInterface>> =
            Arc::new(Mutex::new(database));
        let bitcoin: Arc<Mutex<dyn crate::bitcoin::BitcoinInterface>> = match electrum {
            Some(address) => {
                config.bitcoin_backend =
                    Some(BitcoinBackend::Electrum(crate::config::ElectrumConfig {
                        addr: format!("tcp://{}", address),
                        validate_domain: true,
                    }));
                // Validate the real indexer's regtest genesis before translating
                // the fixture's logical role back to Bitcoin/mainnet admission.
                config.bitcoin_config.network = miniscript::bitcoin::Network::Regtest;
                let backend = crate::setup_electrum(&config, database.clone())
                    .map_err(|_| SubmissionError::BackendUnavailable)?;
                config.bitcoin_config.network = miniscript::bitcoin::Network::Bitcoin;
                Arc::new(Mutex::new(backend))
            }
            None => {
                config.bitcoin_backend = Some(BitcoinBackend::Bitcoind(self.node.clone()));
                Arc::new(Mutex::new(
                    BitcoinD::new(&self.node, "unused-claim-transport-fixture".into())
                        .map_err(|_| SubmissionError::BackendUnavailable)?,
                ))
            }
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let control = DaemonControl::new(
            config.clone(),
            bitcoin,
            sender.clone(),
            database,
            secp,
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let worker = std::thread::spawn(move || {
            while let Ok(message) = receiver.recv() {
                match message {
                    PollerMessage::Shutdown => break,
                    PollerMessage::PollNow(ack) => {
                        let _ = ack.send(());
                    }
                    PollerMessage::PollNowNoAck => {}
                }
            }
        });
        Ok((
            config,
            crate::DaemonHandle::Controller {
                control,
                poller_sender: sender,
                poller_handle: worker,
                scan_abort: Arc::new(AtomicBool::new(false)),
            },
        ))
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
    pub fn submit_ancestry(
        &self,
        verified: &VerifiedAncestryTransfer,
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
