//! Local-node trust boundary: the user trusts their own consensus-validating
//! node, rather than Connect's authenticated anchor. A scheduled fork alone
//! cannot admit a wallet; require a coherent active version-2 chain tip.
use super::*;
use coincube_core::chain::ChainId;

pub fn activation_height(chain: ChainId) -> Option<u64> {
    match chain {
        ChainId::BitcoinBlake2b => Some(961_640),
        ChainId::BitcoinBlake2bTestnet4 => Some(150_308),
        _ => None,
    }
}

fn refused() -> BitcoindError {
    BitcoindError::MalformedResponse(
        "Local node did not verify the selected Bitcoin Blake2b chain".into(),
    )
}

impl BitcoinD {
    /// Check identity before attaching to a running managed node. While syncing,
    /// only the exact configured fork schedule is required; wallet admission
    /// additionally requires completed IBD and a post-fork version-2 tip.
    pub fn check_local_fork_chain(
        &self,
        chain: ChainId,
        wallet_ready: bool,
    ) -> Result<(), BitcoindError> {
        self.validate_local_fork(chain, wallet_ready)
    }

    pub(crate) fn admit_local_fork(mut self, chain: ChainId) -> Result<Self, BitcoindError> {
        self.validate_local_fork(chain, true)?;
        self.local_fork_chain = Some(chain);
        Ok(self)
    }

    pub(super) fn validate_local_fork(
        &self,
        chain: ChainId,
        wallet_ready: bool,
    ) -> Result<(), BitcoindError> {
        let height = activation_height(chain).ok_or_else(refused)?;
        // Re-read cookie credentials after a managed restart. These bounded
        // requests bypass the guarded wallet clients, avoiding recursive admission.
        let client = Self::build_client(
            &self.config,
            &self.watchonly_wallet_path,
            ClientKind::PollNode,
        )?;
        let call = |method: &str, params: Option<&serde_json::value::RawValue>| {
            if self.poll_abort.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(BitcoindError::PollAborted);
            }
            self.try_request(&client, client.build_request(method, params))
        };
        let info = call("getblockchaininfo", None)?;
        let expected = if chain == ChainId::BitcoinBlake2b {
            "main"
        } else {
            "testnet4"
        };
        if info.get("chain").and_then(Json::as_str) != Some(expected) {
            return Err(refused());
        }
        let hash = info
            .get("bestblockhash")
            .and_then(Json::as_str)
            .ok_or_else(refused)?;
        bitcoin::BlockHash::from_str(hash).map_err(|_| refused())?;
        let blocks = info
            .get("blocks")
            .and_then(Json::as_u64)
            .ok_or_else(refused)?;
        let deployment = call("getdeploymentinfo", params!(Json::String(hash.into())))?;
        let fork = deployment.get("blake2b").ok_or_else(refused)?;
        if fork.get("height").and_then(Json::as_u64) != Some(height)
            || fork.get("active").and_then(Json::as_bool).is_none()
            || deployment.get("hash").and_then(Json::as_str) != Some(hash)
            || deployment.get("height").and_then(Json::as_u64) != Some(blocks)
        {
            return Err(refused());
        }
        if wallet_ready {
            let header = call(
                "getblockheader",
                params!(Json::String(hash.into()), Json::Bool(true)),
            )?;
            if info.get("initialblockdownload").and_then(Json::as_bool) != Some(false)
                || info
                    .get("blocks")
                    .and_then(Json::as_u64)
                    .is_none_or(|blocks| blocks < height)
                || fork.get("active").and_then(Json::as_bool) != Some(true)
                || header.get("header_version").and_then(Json::as_u64) != Some(2)
                || header.get("hash").and_then(Json::as_str) != Some(hash)
                || header.get("height").and_then(Json::as_u64) != Some(blocks)
                || header
                    .get("confirmations")
                    .and_then(Json::as_i64)
                    .is_none_or(|n| n <= 0)
            {
                return Err(refused());
            }
        }
        if call("getbestblockhash", None)?.as_str() != Some(hash) {
            return Err(refused());
        }
        Ok(())
    }
}
