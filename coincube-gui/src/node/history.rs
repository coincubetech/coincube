//! Verified transaction handoff and resumable historical wallet scans.
//!
//! Recovery keeps the node's active chainstate. Block bodies are fetched and
//! scanned in bounded ranges; undo data is not required by a wallet rescan.
//! Automatic pruning must be disabled before fetching any missing block.

use std::{
    collections::BTreeMap,
    convert::TryFrom,
    fs::File,
    path::{Path, PathBuf},
    time::Duration,
};

use coincube_core::{
    descriptors::CoincubeDescriptor,
    miniscript::bitcoin::{
        self,
        consensus::encode::{deserialize, serialize_hex},
        hashes::{sha256, Hash},
        BlockHash, MerkleBlock, Network, OutPoint, Txid,
    },
};
use coincubed::config::{BitcoindConfig, EsploraConfig};
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const RESPONSE_LIMIT: usize = 10 * 1024 * 1024;
pub const BATCH_BLOCKS: u32 = 32;
const MIN_FREE_BYTES: u64 = 512 * 1024 * 1024;

/// A transport/RPC failure never means that a transaction or block is absent.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: Option<i64>,
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl From<String> for RpcError {
    fn from(message: String) -> Self {
        Self {
            code: None,
            message,
        }
    }
}

/// Secrets are deliberately excluded from Debug and persisted job records.
pub struct NodeRpc {
    client: reqwest::Client,
    node_url: reqwest::Url,
    wallet_url: reqwest::Url,
    user: String,
    pass: String,
}

impl NodeRpc {
    pub async fn new(
        cfg: &BitcoindConfig,
        wallet: &str,
        chain: crate::chain::ChainId,
    ) -> Result<Self, String> {
        if chain.is_blake2b() {
            return Err("History recovery is available for regular Bitcoin only".into());
        }
        let (user, pass) = crate::app::local_switch::rpc_credentials(cfg).await?;
        let node_url =
            reqwest::Url::parse(&format!("http://{}/", cfg.addr)).map_err(|e| e.to_string())?;
        let mut wallet_url = node_url.clone();
        wallet_url
            .path_segments_mut()
            .map_err(|_| "Invalid node URL")?
            .clear()
            .push("wallet")
            .push(wallet);
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            client,
            node_url,
            wallet_url,
            user,
            pass,
        })
    }

    pub async fn call(&self, wallet: bool, method: &str, params: Value) -> Result<Value, RpcError> {
        let url = if wallet {
            &self.wallet_url
        } else {
            &self.node_url
        };
        let response = self
            .client
            .post(url.clone())
            .basic_auth(&self.user, Some(&self.pass))
            .timeout(Duration::from_secs(if method == "rescanblockchain" {
                120
            } else {
                10
            }))
            .json(&json!({"jsonrpc":"2.0", "id":"history", "method":method, "params":params}))
            .send()
            .await
            .map_err(|_| format!("Cannot reach local node for {method}"))?;
        let bytes = bounded_body(response, RESPONSE_LIMIT).await?;
        let response: Value =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid node response".to_string())?;
        if response["id"].as_str() != Some("history") {
            return Err("Mismatched node response".to_string().into());
        }
        if !response["error"].is_null() {
            return Err(RpcError {
                code: response["error"]["code"].as_i64(),
                message: format!(
                    "Local node refused {method}: {}",
                    response["error"]["message"].as_str().unwrap_or("RPC error")
                ),
            });
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| "Missing node result".to_string().into())
    }

    pub async fn block_hash(&self, height: u32) -> Result<BlockHash, String> {
        self.call(false, "getblockhash", json!([height]))
            .await
            .map_err(|e| e.to_string())?
            .as_str()
            .ok_or("Missing block hash")?
            .parse()
            .map_err(|_| "Invalid block hash".into())
    }

    pub async fn check_chain(&self, network: Network) -> Result<Value, String> {
        if self.block_hash(0).await?
            != bitcoin::blockdata::constants::genesis_block(network).block_hash()
        {
            return Err("The local node serves a different Bitcoin network".into());
        }
        let info = self
            .call(false, "getblockchaininfo", json!([]))
            .await
            .map_err(|e| e.to_string())?;
        if info["initialblockdownload"].as_bool() != Some(false) {
            return Err("Wait for the local node to finish blockchain sync".into());
        }
        Ok(info)
    }

    pub async fn knows_transaction(&self, txid: Txid) -> Result<bool, String> {
        match self
            .call(true, "gettransaction", json!([txid.to_string(), true]))
            .await
        {
            Ok(tx) if tx["txid"].as_str() == Some(txid.to_string().as_str()) => Ok(true),
            Ok(_) => Err("Invalid wallet transaction response".into()),
            Err(e) if e.code == Some(-5) => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }

    pub async fn transaction_at(
        &self,
        txid: Txid,
        height: Option<u32>,
    ) -> Result<Option<bitcoin::Transaction>, String> {
        let value = match self
            .call(true, "gettransaction", json!([txid.to_string(), true]))
            .await
        {
            Ok(value) => value,
            Err(error) if error.code == Some(-5) => return Ok(None),
            Err(error) => return Err(error.to_string()),
        };
        let raw = value["hex"]
            .as_str()
            .ok_or("Wallet did not return the raw transaction")?;
        let bytes = <Vec<u8> as bitcoin::hex::FromHex>::from_hex(raw)
            .map_err(|_| "Invalid wallet transaction hex")?;
        let tx: bitcoin::Transaction =
            deserialize(&bytes).map_err(|_| "Invalid wallet transaction")?;
        if tx.compute_txid() != txid {
            return Err("Wallet returned a different transaction".into());
        }
        if let Some(height) = height {
            if value["confirmations"].as_i64().is_none_or(|n| n <= 0)
                || value["blockhash"].as_str()
                    != Some(self.block_hash(height).await?.to_string().as_str())
            {
                return Ok(None);
            }
        } else if value["confirmations"].as_i64() != Some(0) {
            return Ok(None);
        } else {
            match self
                .call(false, "getmempoolentry", json!([txid.to_string()]))
                .await
            {
                Ok(entry) if entry.is_object() => {}
                Ok(_) => return Err("Invalid mempool transaction response".into()),
                Err(error) if error.code == Some(-5) => return Ok(None),
                Err(error) => return Err(error.to_string()),
            }
        }
        Ok(Some(tx))
    }

    /// An unloaded stale wallet can require block bodies before Core permits
    /// attachment. Only an explicit pruning refusal enters bootstrap mode.
    pub async fn ensure_loaded(&self, wallet: &str) -> Result<bool, String> {
        let loaded = self
            .call(false, "listwallets", json!([]))
            .await
            .map_err(|e| e.to_string())?;
        let loaded = loaded.as_array().ok_or("Cannot read loaded wallets")?;
        if loaded.iter().any(|name| name.as_str() == Some(wallet)) {
            return Ok(true);
        }
        if Path::new(wallet).exists() {
            match self.call(false, "loadwallet", json!([wallet])).await {
                Ok(_) => Ok(true),
                Err(error)
                    if error.code == Some(-4)
                        && error.message.to_ascii_lowercase().contains("prun") =>
                {
                    Ok(false)
                }
                Err(error) => Err(error.to_string()),
            }
        } else {
            self.call(
                false,
                "createwallet",
                json!([wallet, true, true, "", false, true, true]),
            )
            .await
            .map_err(|e| e.to_string())?;
            Ok(true)
        }
    }

    /// Import descriptors with an adequate range before scanning/importing funds.
    /// Existing wallets must be watch-only descriptor wallets with these same
    /// descriptors; never populate an unrelated wallet at the requested path.
    pub async fn prepare_wallet(
        &self,
        wallet: &str,
        descriptor: &CoincubeDescriptor,
        range: u32,
    ) -> Result<(), String> {
        if !self.ensure_loaded(wallet).await? {
            return Err("Download the wallet's catch-up blocks before loading it".into());
        }
        let info = self
            .call(true, "getwalletinfo", json!([]))
            .await
            .map_err(|e| e.to_string())?;
        if info["private_keys_enabled"].as_bool() != Some(false)
            || info["descriptors"].as_bool() != Some(true)
        {
            return Err("Recovery requires Tenshu's watch-only descriptor wallet".into());
        }
        let receive = descriptor.receive_descriptor().to_string();
        let change = descriptor.change_descriptor().to_string();
        let existing = self
            .call(true, "listdescriptors", json!([]))
            .await
            .map_err(|e| e.to_string())?;
        let entries = existing["descriptors"]
            .as_array()
            .ok_or("Cannot read wallet descriptors")?;
        if entries
            .iter()
            .any(|d| !matches!(d["desc"].as_str(), Some(s) if s == receive || s == change))
        {
            return Err("The local wallet contains different descriptors".into());
        }
        let range = entries
            .iter()
            .filter_map(|entry| entry["range"][1].as_u64())
            .max()
            .unwrap_or(0)
            .max(u64::from(range));
        let result = self.call(true, "importdescriptors", json!([[{"desc":receive,"timestamp":"now","range":[0,range]}, {"desc":change,"timestamp":"now","range":[0,range]}]])).await.map_err(|e| e.to_string())?;
        if !result
            .as_array()
            .is_some_and(|a| a.len() == 2 && a.iter().all(|r| r["success"].as_bool() == Some(true)))
        {
            return Err("Could not prepare the wallet descriptors for recovery".into());
        }
        Ok(())
    }

    /// Core's pruneheight includes undo availability. Wallet rescans only need
    /// block bodies, so query actual bodies instead of using that height.
    async fn has_block(&self, hash: BlockHash) -> Result<bool, String> {
        match self
            .call(false, "getblock", json!([hash.to_string(), 0]))
            .await
        {
            Ok(raw) => {
                let hex = raw.as_str().ok_or("Invalid block body response")?;
                let bytes = <Vec<u8> as bitcoin::hex::FromHex>::from_hex(hex)
                    .map_err(|_| "Invalid block body hex")?;
                let block: bitcoin::Block =
                    deserialize(&bytes).map_err(|_| "Invalid block body")?;
                if block.block_hash() != hash
                    || !block.check_merkle_root()
                    || !block.check_witness_commitment()
                {
                    return Err("The downloaded block does not match the local chain".into());
                }
                Ok(true)
            }
            Err(e) if e.code == Some(-1) && e.message.contains("pruned") => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }

    async fn fetch_block(&self, hash: BlockHash, attempt: u32) -> Result<bool, String> {
        if self.has_block(hash).await? {
            return Ok(true);
        }
        let peers = self
            .call(false, "getpeerinfo", json!([]))
            .await
            .map_err(|e| e.to_string())?;
        let peers = peers.as_array().ok_or("Cannot read node peers")?;
        let mut ids = Vec::new();
        for peer in peers
            .iter()
            .filter(|p| p["inbound"].as_bool() == Some(false))
        {
            let Some(services) = peer["services"]
                .as_str()
                .and_then(|s| u64::from_str_radix(s, 16).ok())
            else {
                continue;
            };
            if services & 9 != 9 {
                continue;
            } // NODE_NETWORK + NODE_WITNESS
            let Some(id) = peer["id"].as_i64() else {
                continue;
            };
            ids.push(id);
        }
        let mut scheduled = false;
        if !ids.is_empty() {
            let offset = attempt as usize % ids.len();
            ids.rotate_left(offset);
        }
        for id in ids {
            if self
                .call(false, "getblockfrompeer", json!([hash.to_string(), id]))
                .await
                .is_ok()
            {
                scheduled = true;
                break;
            }
        }
        if !scheduled {
            return Err("No archival peer is available to recover old blocks; retry after the node connects to peers".into());
        }
        // The RPC schedules a request; it does not acknowledge receipt. Yield
        // with an unchanged checkpoint when a body has not arrived yet.
        tokio::time::sleep(Duration::from_millis(500)).await;
        self.has_block(hash).await
    }
}

async fn bounded_body(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err("Node/provider response is too large".into());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "Cannot read node/provider response")?
    {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err("Node/provider response is too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// An inclusion proof is verified before being sent to the wallet. Local
/// getblockhash supplies the active-chain authority; provider metadata does not.
pub fn verify_proof(raw: &[u8], txid: Txid, block: BlockHash) -> Result<MerkleBlock, String> {
    let proof: MerkleBlock = deserialize(raw).map_err(|_| "Invalid transaction inclusion proof")?;
    let mut matches = vec![];
    let mut indexes = vec![];
    let root = proof
        .txn
        .extract_matches(&mut matches, &mut indexes)
        .map_err(|_| "Invalid proof merkle tree")?;
    if proof.header.block_hash() != block
        || root != proof.header.merkle_root
        || !matches.contains(&txid)
    {
        return Err("Transaction proof does not match the local active chain".into());
    }
    Ok(proof)
}

pub async fn fetch_proof(
    config: &EsploraConfig,
    txid: Txid,
    block: BlockHash,
) -> Result<Vec<u8>, String> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let providers = [
        Some((&config.addr, config.token.as_ref())),
        config
            .fallback_addr
            .as_ref()
            .map(|a| (a, config.fallback_token.as_ref())),
        config
            .secondary_fallback_addr
            .as_ref()
            .map(|a| (a, config.secondary_fallback_token.as_ref())),
    ];
    for (base, token) in IntoIterator::into_iter(providers).flatten() {
        let mut request = client.get(format!(
            "{}/tx/{txid}/merkleblock-proof",
            base.trim_end_matches('/')
        ));
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let Ok(response) = request.send().await else {
            continue;
        };
        if !response.status().is_success() {
            continue;
        }
        let Ok(raw) = bounded_body(response, 1024 * 1024).await else {
            continue;
        };
        let Ok(hex) = std::str::from_utf8(&raw) else {
            continue;
        };
        if let Ok(bytes) = <Vec<u8> as bitcoin::hex::FromHex>::from_hex(hex.trim()) {
            if verify_proof(&bytes, txid, block).is_ok() {
                return Ok(bytes);
            }
        }
    }
    Err("No configured Connect/Esplora provider supplied an inclusion proof; recover this transaction's block instead".into())
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub genesis: BlockHash,
    pub node_instance: String,
    pub endpoint: String,
    pub wallet: String,
    pub descriptor: String,
}

impl std::fmt::Debug for Binding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Binding")
            .field("genesis", &self.genesis)
            .field("node_instance", &self.node_instance)
            .field("endpoint", &self.endpoint)
            .field("wallet", &self.wallet)
            .field("descriptor", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    FetchingForLoad,
    Downloading,
    Scanning,
    Reconciling,
    Paused,
    Complete,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recovery {
    pub version: u32,
    pub id: String,
    pub binding: Binding,
    pub start: u32,
    pub next: u32,
    pub target: u32,
    pub target_hash: BlockHash,
    pub phase: Phase,
    pub fetch_attempts: u32,
    pub last_error: Option<String>,
    pub download_next: u32,
    pub wallet_ready: bool,
    pub import_requested: bool,
    #[serde(default)]
    pub db_replayed: bool,
}

impl Recovery {
    pub fn new(
        binding: Binding,
        start: u32,
        target: u32,
        target_hash: BlockHash,
    ) -> Result<Self, String> {
        if start > target || target == u32::MAX {
            return Err("Recovery height must be at or below the current chain tip".into());
        }
        Ok(Self {
            version: 1,
            id: uuid::Uuid::new_v4().to_string(),
            binding,
            start,
            next: start,
            target,
            target_hash,
            phase: Phase::Downloading,
            fetch_attempts: 0,
            last_error: None,
            download_next: start,
            wallet_ready: false,
            import_requested: false,
            db_replayed: false,
        })
    }
    pub fn validate(&self, binding: &Binding) -> Result<(), String> {
        if self.version != 1
            || &self.binding != binding
            || self.start > self.next
            || self.next > self.target.saturating_add(1)
            || self.target == u32::MAX
            || self.start > self.target
            || (self.phase == Phase::Complete && self.next != self.target + 1)
            || self.download_next < self.start
            || self.download_next > self.target.saturating_add(1)
        {
            return Err(
                "Recovery progress belongs to a different wallet/node or is invalid".into(),
            );
        }
        Ok(())
    }
    pub fn progress(&self) -> f32 {
        (self.next - self.start) as f32 / (self.target - self.start + 1) as f32
    }
}

/// Per-network controller lock: recovery and pruning cannot run concurrently,
/// even across multiple Cubes, windows or processes.
pub struct HistoryLease(File);
impl HistoryLease {
    pub fn acquire(directory: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("controller.lock"))
            .map_err(|e| e.to_string())?;
        if !file.try_lock_exclusive().map_err(|e| e.to_string())? {
            return Err(
                "Another wallet is recovering or pruning this node; retry when it finishes".into(),
            );
        }
        Ok(Self(file))
    }
}
impl Drop for HistoryLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if metadata.len() > 1024 * 1024 {
        return Err("History progress file is too large".into());
    }
    serde_json::from_slice(&std::fs::read(path).map_err(|e| e.to_string())?)
        .map(Some)
        .map_err(|e| e.to_string())
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    crate::node::managed_conf::write_conf_atomically(path, &bytes).map_err(|e| e.to_string())
}

pub fn recovery_path(directory: &Path, wallet: &str) -> PathBuf {
    directory.join(format!("{}.json", sha256::Hash::hash(wallet.as_bytes())))
}

/// A persisted checkpoint advances only after an acknowledged, successful
/// bounded scan. RPC timeouts leave the batch repeatable; they never mean done.
pub async fn bootstrap_step(
    rpc: &NodeRpc,
    network: Network,
    binding: &Binding,
    path: &Path,
) -> Result<Recovery, String> {
    let dir = path.parent().ok_or("Missing recovery directory")?;
    let _lease = HistoryLease::acquire(dir)?;
    let mut job: Recovery = read_json(path)?.ok_or("Missing recovery progress")?;
    job.validate(binding)?;
    let result:Result<(),String>=async {
        let info=rpc.check_chain(network).await?;
        if info["automatic_pruning"].as_bool()!=Some(false) || info["pruned"].as_bool()!=Some(true) {return Err("Restart the managed node with recovery pruning controls".into());}
        let anchor=rpc.block_hash(job.target).await?;
        if anchor!=job.target_hash {job.download_next=job.start;job.next=job.start;job.target_hash=anchor;job.db_replayed=false;}
        let end=job.download_next.saturating_add(BATCH_BLOCKS-1).min(job.target);
        for height in job.download_next..=end {
            if fs4::available_space(dir).map_err(|e|e.to_string())?<MIN_FREE_BYTES {return Err("Free more disk space to download the blocks needed to load this wallet, then resume".into());}
            if !rpc.fetch_block(rpc.block_hash(height).await?,job.fetch_attempts).await? {
                job.fetch_attempts+=1;
                if job.fetch_attempts>=8 {return Err("An archival peer did not supply the wallet's catch-up block; retry recovery".into());}
                return Ok(());
            }
            job.download_next=height+1;
            job.fetch_attempts=0;
            write_json(path,&job)?;
        }
        Ok(())
    }.await;
    if let Err(error) = result {
        job.phase = Phase::Paused;
        job.last_error = Some(error.clone());
        write_json(path, &job)?;
        return Err(error);
    }
    write_json(path, &job)?;
    Ok(job)
}

pub async fn recover_step(
    rpc: &NodeRpc,
    network: Network,
    binding: &Binding,
    path: &Path,
) -> Result<Recovery, String> {
    let directory = path.parent().ok_or("Missing recovery directory")?;
    let _lease = HistoryLease::acquire(directory)?;
    let mut recovery: Recovery = read_json(path)?.ok_or("No saved wallet recovery to resume")?;
    recovery.validate(binding)?;
    if matches!(recovery.phase, Phase::Paused | Phase::Cancelled) {
        return Ok(recovery);
    }
    let result: Result<(),String> = async {
        let info = rpc.check_chain(network).await?;
        if rpc.block_hash(recovery.target).await? != recovery.target_hash {
            // Every previously scanned block may be on the old branch. Rewind
            // the entire requested range; stale checkpoints cannot authorize a
            // switch. Keep the user's range and preserve wallet records.
            recovery.next=recovery.start;
            recovery.db_replayed=false;
            recovery.target_hash=rpc.block_hash(recovery.target).await?;
            recovery.phase=Phase::Downloading;
            write_json(path,&recovery)?;
        }
        if matches!(recovery.phase,Phase::Complete|Phase::Reconciling) { return Ok(()); }
        if info["pruned"].as_bool()!=Some(true) || info["automatic_pruning"].as_bool()!=Some(false) {
            return Err("Restart the managed node with recovery pruning controls before downloading blocks".into());
        }
        let wallet = rpc.call(true,"getwalletinfo",json!([])).await.map_err(|e|e.to_string())?;
        if wallet["scanning"].is_object() { return Ok(()); }
        if wallet["scanning"].as_bool()!=Some(false) { return Err("Cannot determine local wallet scan status".into()); }
        let end=recovery.next.saturating_add(BATCH_BLOCKS-1).min(recovery.target);
        recovery.phase=Phase::Downloading;
        write_json(path,&recovery)?;
        let mut hashes=Vec::new();
        for height in recovery.next..=end {
            if fs4::available_space(directory).map_err(|e|e.to_string())? < MIN_FREE_BYTES { return Err("Not enough free disk space to recover more blocks; free space and resume".into()); }
            let hash=rpc.block_hash(height).await?;
            if !rpc.fetch_block(hash,recovery.fetch_attempts).await? {
                recovery.fetch_attempts+=1;
                if recovery.fetch_attempts>=8 { return Err("An archival peer did not supply the requested block; retry recovery after checking node connectivity".into()); }
                return Ok(());
            }
            hashes.push((height,hash));
        }
        recovery.phase=Phase::Scanning;
        write_json(path,&recovery)?;
        // The final scan omits stop_height to also reconcile transactions that
        // were already in the mempool before their funding parents were imported.
        let params=if end==recovery.target { json!([recovery.next]) } else {json!([recovery.next,end])};
        let scanned=rpc.call(true,"rescanblockchain",params).await.map_err(|e|e.to_string())?;
        if scanned["start_height"].as_u64()!=Some(u64::from(recovery.next)) || scanned["stop_height"].as_u64().is_none_or(|h|h<u64::from(end)) {
            return Err("The wallet did not finish scanning the requested blocks; resume recovery".into());
        }
        for (height,hash) in hashes { if rpc.block_hash(height).await? != hash { return Err("The chain changed during recovery; resume to scan the new branch".into()); } }
        recovery.next=end+1;
        recovery.fetch_attempts=0;
        recovery.phase=if end==recovery.target {Phase::Reconciling} else {Phase::Downloading};
        Ok(())
    }.await;
    if let Err(error) = result {
        recovery.phase = Phase::Paused;
        recovery.last_error = Some(error.clone());
        write_json(path, &recovery)?;
        return Err(error);
    }
    recovery.last_error = None;
    write_json(path, &recovery)?;
    Ok(recovery)
}

/// Import known confirmed transactions in chain order. Unsupported imports
/// (including pure outgoing transactions) return block heights for scanning.
/// Missing proofs likewise fall back to scanning, never dropping old records.
pub async fn import_transactions(
    rpc: &NodeRpc,
    source: &EsploraConfig,
    transactions: &[coincubed::commands::TransactionInfo],
) -> Result<Vec<u32>, String> {
    let mut ordered: Vec<_> = transactions
        .iter()
        .filter_map(|tx| {
            tx.height
                .and_then(|h| u32::try_from(h).ok())
                .map(|height| (height, tx))
        })
        .collect();
    ordered.sort_by_key(|(height, _)| *height);
    let mut recover = BTreeMap::new();
    for (height, tx) in ordered {
        let txid = tx.tx.compute_txid();
        if rpc.transaction_at(txid, Some(height)).await?.is_some() {
            continue;
        }
        let block = rpc.block_hash(height).await?;
        let proof = match fetch_proof(source, txid, block).await {
            Ok(p) => p,
            Err(_) => {
                recover.insert(height, ());
                continue;
            }
        };
        let proof = verify_proof(&proof, txid, block)?;
        match rpc
            .call(
                true,
                "importprunedfunds",
                json!([serialize_hex(&tx.tx), serialize_hex(&proof)]),
            )
            .await
        {
            Ok(_) if rpc.transaction_at(txid, Some(height)).await?.is_some() => {}
            Ok(_) => return Err("Imported transaction is missing from the local wallet".into()),
            Err(e) if e.code == Some(-5) => {
                recover.insert(height, ());
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(recover.into_keys().collect())
}

/// Proofs do not establish unspentness. A vanished output requires a scan to
/// find its spender, unless the known spender is already recorded locally.
pub async fn reconcile_coins(
    rpc: &NodeRpc,
    coins: &[coincubed::commands::ListCoinsEntry],
) -> Result<Option<u32>, String> {
    let mut earliest = None;
    for coin in coins {
        let height = coin
            .block_height
            .map(|height| u32::try_from(height).map_err(|_| "Invalid confirmed coin height"))
            .transpose()?;
        if height.is_none() {
            let funding = rpc.transaction_at(coin.outpoint.txid, None).await?;
            if funding.is_none() {
                return Err("Connect has an unconfirmed transaction the local node has not seen. Keep Connect active until the nodes agree, then resume recovery.".into());
            }
            if let Some(spend) = coin.spend_info {
                if rpc
                    .transaction_at(spend.txid, spend.height.and_then(|h| u32::try_from(h).ok()))
                    .await?
                    .is_none_or(|tx| {
                        !tx.input
                            .iter()
                            .any(|input| input.previous_output == coin.outpoint)
                    })
                {
                    return Err("The local node has not seen this pending spend. Keep Connect active until the nodes agree, then resume recovery.".into());
                }
            }
            let unspent = rpc
                .call(
                    false,
                    "gettxout",
                    json!([coin.outpoint.txid.to_string(), coin.outpoint.vout, true]),
                )
                .await
                .map_err(|e| e.to_string())?;
            if unspent.is_null() != coin.spend_info.is_some() {
                return Err("Connect and the local node disagree about a pending output's spender. Keep Connect active and resume when they agree.".into());
            }
            continue;
        }
        let height = height.expect("unconfirmed handled above");
        let funding = rpc.transaction_at(coin.outpoint.txid, Some(height)).await?;
        let mut needs_scan = funding.is_none();
        let OutPoint { txid, vout } = coin.outpoint;
        let unspent = rpc
            .call(false, "gettxout", json!([txid.to_string(), vout, true]))
            .await
            .map_err(|e| e.to_string())?;
        if let Some(spend) = coin.spend_info {
            let spender = rpc
                .transaction_at(spend.txid, spend.height.and_then(|h| u32::try_from(h).ok()))
                .await?;
            needs_scan |= spender.is_none_or(|tx| {
                !tx.input
                    .iter()
                    .any(|input| input.previous_output == coin.outpoint)
            }) || !unspent.is_null();
        } else {
            needs_scan |= unspent.is_null();
            if !unspent.is_null() {
                let output = funding.as_ref().and_then(|tx| tx.output.get(vout as usize));
                let script = unspent["scriptPubKey"]["hex"]
                    .as_str()
                    .ok_or("Invalid unspent output script")?;
                let amount = unspent["value"]
                    .as_f64()
                    .and_then(|value| bitcoin::Amount::from_btc(value).ok())
                    .ok_or("Invalid unspent output amount")?;
                needs_scan |= output.is_none_or(|output| {
                    output.value != amount
                        || output.script_pubkey.to_hex_string() != script
                        || amount != coin.amount
                });
            }
        }
        if needs_scan {
            earliest = Some(earliest.map_or(height, |h: u32| h.min(height)));
        }
    }
    Ok(earliest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;

    fn genesis() -> bitcoin::Block {
        bitcoin::blockdata::constants::genesis_block(Network::Regtest)
    }
    fn binding() -> Binding {
        Binding {
            genesis: genesis().block_hash(),
            node_instance: "test-node".into(),
            endpoint: "fixture".into(),
            wallet: "history-fixture".into(),
            descriptor: "fixture".into(),
        }
    }
    fn directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!("history-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    async fn rpc(server: &MockServer) -> NodeRpc {
        NodeRpc::new(
            &BitcoindConfig {
                addr: *server.address(),
                rpc_auth: coincubed::config::BitcoindRpcAuth::UserPass(
                    "fixture".into(),
                    "fixture".into(),
                ),
            },
            "history-fixture",
            crate::chain::ChainId::from(Network::Regtest),
        )
        .await
        .unwrap()
    }
    async fn response(server: &MockServer, method: &str, result: Value) {
        server
            .mock_async(|when, then| {
                when.method(POST)
                    .json_body_partial(json!({"method":method}).to_string());
                then.status(200)
                    .json_body(json!({"id":"history","result":result}));
            })
            .await;
    }
    async fn base(server: &MockServer, automatic: bool) {
        response(
            server,
            "getblockhash",
            json!(genesis().block_hash().to_string()),
        )
        .await;
        response(server,"getblockchaininfo",json!({"pruned":true,"automatic_pruning":automatic,"initialblockdownload":false,"blocks":0,"pruneheight":999})).await;
        response(server, "getwalletinfo", json!({"scanning":false})).await;
        response(server, "getblock", json!(serialize_hex(&genesis()))).await;
    }

    #[test]
    fn inclusion_proof_requires_transaction_and_active_chain_block() {
        let block = genesis();
        let txid = block.txdata[0].compute_txid();
        let proof = MerkleBlock::from_block_with_predicate(&block, |t| *t == txid);
        let bytes = bitcoin::consensus::serialize(&proof);
        assert!(verify_proof(&bytes, txid, block.block_hash()).is_ok());
        assert!(verify_proof(&bytes, Txid::from_byte_array([42; 32]), block.block_hash()).is_err());
        assert!(verify_proof(&bytes, txid, BlockHash::from_byte_array([42; 32])).is_err());
        assert!(verify_proof(&[0; 8], txid, block.block_hash()).is_err());
        let mut bad = proof;
        bad.header.merkle_root = bitcoin::TxMerkleNode::from_byte_array([42; 32]);
        assert!(verify_proof(
            &bitcoin::consensus::serialize(&bad),
            txid,
            bad.header.block_hash()
        )
        .is_err());
    }

    #[test]
    fn checkpoint_is_scoped_and_bounds_checked() {
        let mut recovery = Recovery::new(binding(), 100, 200, genesis().block_hash()).unwrap();
        assert!(recovery.validate(&binding()).is_ok());
        for field in ["wallet", "descriptor", "endpoint", "node_instance"] {
            let mut other = binding();
            match field {
                "wallet" => other.wallet.push('x'),
                "descriptor" => other.descriptor.push('x'),
                "endpoint" => other.endpoint.push('x'),
                _ => other.node_instance.push('x'),
            };
            assert!(recovery.validate(&other).is_err());
        }
        let mut other = binding();
        other.genesis = BlockHash::from_byte_array([42; 32]);
        assert!(recovery.validate(&other).is_err());
        recovery.next = 99;
        assert!(recovery.validate(&binding()).is_err());
        recovery.next = 202;
        assert!(recovery.validate(&binding()).is_err());
        assert!(Recovery::new(binding(), 201, 200, genesis().block_hash()).is_err());
    }

    #[test]
    fn progress_storage_is_atomic_and_controller_excludes_other_wallets() {
        let dir = directory();
        let path = recovery_path(&dir, "fixture");
        let mut recovery = Recovery::new(binding(), 0, 100, genesis().block_hash()).unwrap();
        write_json(&path, &recovery).unwrap();
        let first = HistoryLease::acquire(&dir).unwrap();
        assert!(HistoryLease::acquire(&dir).is_err());
        recovery.next = 32;
        write_json(&path, &recovery).unwrap();
        let read: Recovery = read_json(&path).unwrap().unwrap();
        assert_eq!(read.next, 32);
        assert!(read_json::<Recovery>(&dir.join("missing"))
            .unwrap()
            .is_none());
        drop(first);
        assert!(HistoryLease::acquire(&dir).is_ok());
        std::fs::write(&path, b"{broken").unwrap();
        assert!(read_json::<Recovery>(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn recovery_reads_block_bodies_despite_unchanged_pruneheight() {
        let server = MockServer::start_async().await;
        base(&server, false).await;
        response(
            &server,
            "rescanblockchain",
            json!({"start_height":0,"stop_height":0}),
        )
        .await;
        let dir = directory();
        let path = recovery_path(&dir, "fixture");
        write_json(
            &path,
            &Recovery::new(binding(), 0, 0, genesis().block_hash()).unwrap(),
        )
        .unwrap();
        let result = recover_step(&rpc(&server).await, Network::Regtest, &binding(), &path)
            .await
            .unwrap();
        assert_eq!(result.phase, Phase::Reconciling);
        assert_eq!(result.next, 1);
        assert_eq!(result.progress(), 1.0);
        let saved: Recovery = read_json(&path).unwrap().unwrap();
        assert_eq!(saved.phase, Phase::Reconciling);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn automatic_pruning_and_short_or_aborted_scans_never_advance_progress() {
        for automatic in [true, false] {
            for stop in [Value::Null, json!(-1)] {
                let server = MockServer::start_async().await;
                base(&server, automatic).await;
                let scan = server
                    .mock_async(|when, then| {
                        when.method(POST)
                            .json_body_partial(r#"{"method":"rescanblockchain"}"#);
                        then.status(200).json_body(
                            json!({"id":"history","result":{"start_height":0,"stop_height":stop}}),
                        );
                    })
                    .await;
                let dir = directory();
                let path = recovery_path(&dir, "fixture");
                write_json(
                    &path,
                    &Recovery::new(binding(), 0, 0, genesis().block_hash()).unwrap(),
                )
                .unwrap();
                assert!(
                    recover_step(&rpc(&server).await, Network::Regtest, &binding(), &path)
                        .await
                        .is_err()
                );
                let saved: Recovery = read_json(&path).unwrap().unwrap();
                assert_eq!(saved.next, 0);
                assert_eq!(saved.phase, Phase::Paused);
                scan.assert_hits_async(if automatic { 0 } else { 1 }).await;
                std::fs::remove_dir_all(dir).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn funding_inclusion_does_not_establish_unspentness_and_stale_spends_are_rejected() {
        let funding = genesis().txdata[0].clone();
        let txid = funding.compute_txid();
        let mut spender = funding.clone();
        spender.input[0].previous_output = OutPoint { txid, vout: 0 };
        let spendid = spender.compute_txid();
        for stale_spend in [false, true] {
            let server = MockServer::start_async().await;
            response(
                &server,
                "getblockhash",
                json!(genesis().block_hash().to_string()),
            )
            .await;
            response(&server, "gettxout", Value::Null).await;
            for (id, tx, confirmations) in [(txid, &funding, 1), (spendid, &spender, 0)] {
                server.mock_async(|when,then| {
                    when.method(POST).json_body_partial(json!({"method":"gettransaction","params":[id.to_string(),true]}).to_string());
                    then.status(200).json_body(json!({"id":"history","result":{"txid":id.to_string(),"hex":serialize_hex(tx),"confirmations":confirmations,"blockhash":genesis().block_hash().to_string()}}));
                }).await;
            }
            server.mock_async(|when,then| {
                when.method(POST).json_body_partial(r#"{"method":"getmempoolentry"}"#);
                then.status(500).json_body(json!({"id":"history","error":{"code":-5,"message":"Transaction not in mempool"}}));
            }).await;
            let coin = coincubed::commands::ListCoinsEntry {
                amount: funding.output[0].value,
                outpoint: OutPoint { txid, vout: 0 },
                address: bitcoin::Address::p2wsh(&bitcoin::ScriptBuf::new(), Network::Regtest),
                block_height: Some(0),
                derivation_index: bitcoin::bip32::ChildNumber::from_normal_idx(0).unwrap(),
                spend_info: if stale_spend {
                    Some(coincubed::commands::LCSpendInfo {
                        txid: spendid,
                        height: None,
                    })
                } else {
                    None
                },
                is_immature: false,
                is_change: false,
                is_from_self: false,
            };
            assert_eq!(
                reconcile_coins(&rpc(&server).await, &[coin]).await.unwrap(),
                Some(0)
            );
        }
    }

    #[tokio::test]
    async fn bootstrap_downloads_without_publishing_a_scan_checkpoint() {
        let server = MockServer::start_async().await;
        base(&server, false).await;
        let dir = directory();
        let path = recovery_path(&dir, "fixture");
        let mut job = Recovery::new(binding(), 0, 0, genesis().block_hash()).unwrap();
        job.phase = Phase::FetchingForLoad;
        write_json(&path, &job).unwrap();
        let job = bootstrap_step(&rpc(&server).await, Network::Regtest, &binding(), &path)
            .await
            .unwrap();
        assert_eq!(job.next, 0);
        assert_eq!(job.download_next, 1);
        assert_eq!(job.phase, Phase::FetchingForLoad);
        assert!(!job.wallet_ready);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn chain_reorganization_rewinds_a_completed_scan_before_reconciliation() {
        let server = MockServer::start_async().await;
        base(&server, false).await;
        response(
            &server,
            "rescanblockchain",
            json!({"start_height":0,"stop_height":0}),
        )
        .await;
        let dir = directory();
        let path = recovery_path(&dir, "fixture");
        let mut job = Recovery::new(binding(), 0, 0, BlockHash::from_byte_array([42; 32])).unwrap();
        job.next = 1;
        job.phase = Phase::Complete;
        job.db_replayed = true;
        write_json(&path, &job).unwrap();
        let job = recover_step(&rpc(&server).await, Network::Regtest, &binding(), &path)
            .await
            .unwrap();
        assert_eq!(job.target_hash, genesis().block_hash());
        assert_eq!(job.phase, Phase::Reconciling);
        assert_eq!(job.next, 1);
        assert!(!job.db_replayed);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn invalid_primary_proof_falls_back_without_disclosing_its_token() {
        let primary = MockServer::start_async().await;
        let fallback = MockServer::start_async().await;
        let block = genesis();
        let txid = block.txdata[0].compute_txid();
        let path = format!("/tx/{txid}/merkleblock-proof");
        let rejected = primary
            .mock_async(|when, then| {
                when.method(GET)
                    .path(&path)
                    .header("authorization", "Bearer primary-fixture");
                then.status(200).body("0000");
            })
            .await;
        let proof = MerkleBlock::from_block_with_predicate(&block, |t| *t == txid);
        let accepted = fallback
            .mock_async(|when, then| {
                when.method(GET)
                    .path(&path)
                    .header("authorization", "Bearer fallback-fixture");
                then.status(200).body(serialize_hex(&proof));
            })
            .await;
        let config: EsploraConfig = serde_json::from_value(json!({
            "addr":primary.base_url(), "token":"primary-fixture",
            "fallback_addr":fallback.base_url(), "fallback_token":"fallback-fixture"
        }))
        .unwrap();
        let bytes = fetch_proof(&config, txid, block.block_hash())
            .await
            .unwrap();
        assert!(verify_proof(&bytes, txid, block.block_hash()).is_ok());
        rejected.assert_async().await;
        accepted.assert_async().await;
    }

    #[tokio::test]
    async fn failed_scan_response_preserves_a_resumable_checkpoint() {
        let server = MockServer::start_async().await;
        base(&server, false).await;
        server
            .mock_async(|when, then| {
                when.method(POST)
                    .json_body_partial(r#"{"method":"rescanblockchain"}"#);
                then.status(503).body("connection interrupted");
            })
            .await;
        let dir = directory();
        let path = recovery_path(&dir, "fixture");
        write_json(
            &path,
            &Recovery::new(binding(), 0, 0, genesis().block_hash()).unwrap(),
        )
        .unwrap();
        assert!(
            recover_step(&rpc(&server).await, Network::Regtest, &binding(), &path)
                .await
                .is_err()
        );
        let saved: Recovery = read_json(&path).unwrap().unwrap();
        assert_eq!(saved.next, 0);
        assert_eq!(saved.phase, Phase::Paused);
        assert!(saved.last_error.is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn rpc_failures_do_not_become_missing_transactions() {
        let server = MockServer::start_async().await;
        let failed = server
            .mock_async(|when, then| {
                when.method(POST);
                then.status(500)
                    .json_body(json!({"id":"history","error":{"code":-28,"message":"Loading"}}));
            })
            .await;
        let client = rpc(&server).await;
        assert!(client
            .knows_transaction(genesis().txdata[0].compute_txid())
            .await
            .is_err());
        failed.assert_async().await;
    }

    #[tokio::test]
    async fn unknown_transaction_is_absence_but_mismatched_response_is_not() {
        for id in ["history", "other-request"] {
            let server = MockServer::start_async().await;
            server
                .mock_async(|when, then| {
                    when.method(POST);
                    then.status(200).json_body(
                        json!({"id":id,"error":{"code":-5,"message":"Unknown transaction"}}),
                    );
                })
                .await;
            let result = rpc(&server)
                .await
                .knows_transaction(genesis().txdata[0].compute_txid())
                .await;
            if id == "history" {
                assert!(!result.unwrap());
            } else {
                assert!(result.is_err());
            }
        }
    }
}
