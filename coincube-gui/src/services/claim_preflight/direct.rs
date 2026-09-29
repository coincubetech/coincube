//! Bounded local-node policy observation. No broadcast or Claim authority.
use super::{request_error, Error, FreshnessPolicy, NodePolicy, DEADLINE, MAX_RESPONSE_BYTES};
use crate::services::claim_observation::CollectionContext;
use coincube_core::miniscript::bitcoin::{consensus, BlockHash, Transaction, Txid, Wtxid};
use coincubed::config::{BitcoindConfig, BitcoindRpcAuth};
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::json;
use std::{convert::TryInto, net::SocketAddr};
use tokio::io::AsyncReadExt;

/// Kept separate from operator evidence: this names the configured RPC node.
/// Construction is private; policy acceptance grants no submission capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectEvidence {
    pub(super) node: SocketAddr,
    pub(super) txid: Txid,
    pub(super) wtxid: Wtxid,
    pub(super) tip: BlockHash,
    pub(super) generation: u64,
    pub(super) observed_at: i64,
    pub(super) policy: NodePolicy,
}
impl DirectEvidence {
    pub fn node(&self) -> SocketAddr {
        self.node
    }
    pub fn txid(&self) -> Txid {
        self.txid
    }
    pub fn wtxid(&self) -> Wtxid {
        self.wtxid
    }
    pub fn tip(&self) -> BlockHash {
        self.tip
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn observed_at(&self) -> i64 {
        self.observed_at
    }
    pub fn node_policy(&self) -> &NodePolicy {
        &self.policy
    }
}

#[derive(Deserialize)]
struct Envelope<T> {
    id: u64,
    result: Option<T>,
    error: Option<serde_json::Value>,
}
#[derive(Deserialize)]
struct Acceptance {
    txid: Txid,
    wtxid: Wtxid,
    allowed: bool,
    #[serde(rename = "reject-reason")]
    reject_reason: Option<String>,
}
fn acceptance(rows: Vec<Acceptance>, tx: &Transaction) -> Result<NodePolicy, Error> {
    let [row]: [Acceptance; 1] = rows.try_into().map_err(|_| Error::InvalidResponse)?;
    if row.txid != tx.compute_txid() || row.wtxid != tx.compute_wtxid() {
        return Err(Error::InvalidResponse);
    }
    match (row.allowed, row.reject_reason) {
        (true, None) => Ok(NodePolicy::Accepted),
        (false, Some(reason)) if !reason.is_empty() && reason.len() <= 2048 => {
            Ok(NodePolicy::Rejected { reason })
        }
        _ => Err(Error::InvalidResponse),
    }
}

async fn credentials(config: &BitcoindConfig) -> Result<(String, String), Error> {
    match &config.rpc_auth {
        BitcoindRpcAuth::UserPass(user, pass) => Ok((user.clone(), pass.clone())),
        BitcoindRpcAuth::CookieFile(path) => {
            let file = tokio::fs::File::open(path)
                .await
                .map_err(|_| Error::Transport)?;
            let mut bytes = Vec::new();
            file.take(16_385)
                .read_to_end(&mut bytes)
                .await
                .map_err(|_| Error::Transport)?;
            if bytes.len() > 16_384 {
                return Err(Error::InvalidRequest);
            }
            let text = std::str::from_utf8(&bytes).map_err(|_| Error::InvalidRequest)?;
            let (user, pass) = text
                .trim_end_matches(['\r', '\n'])
                .split_once(':')
                .ok_or(Error::InvalidRequest)?;
            if user.is_empty() || pass.is_empty() {
                return Err(Error::InvalidRequest);
            }
            Ok((user.to_owned(), pass.to_owned()))
        }
    }
}
async fn rpc<T: DeserializeOwned>(
    client: &reqwest::Client,
    node: SocketAddr,
    auth: &(String, String),
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<T, Error> {
    let mut response = client
        .post(format!("http://{}/", node))
        .basic_auth(&auth.0, Some(&auth.1))
        .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
        .send()
        .await
        .map_err(request_error)?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(Error::Http {
            status: response.status().as_u16(),
            retry_after_seconds: None,
        });
    }
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
    {
        return Err(Error::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(request_error)? {
        if chunk.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
            return Err(Error::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    let envelope: Envelope<T> =
        serde_json::from_slice(&bytes).map_err(|_| Error::InvalidResponse)?;
    if envelope.id != id || envelope.error.is_some() {
        return Err(Error::InvalidResponse);
    }
    envelope.result.ok_or(Error::InvalidResponse)
}

/// Read-only, single-attempt RPCs against the explicit configured node. The
/// caller supplies the independently observed Bitcoin tip. No wallet load,
/// fallback, retry, persistence or sendrawtransaction occurs here.
pub async fn observe(
    config: BitcoindConfig,
    transaction: Transaction,
    expected_tip: BlockHash,
    context: CollectionContext,
    policy: FreshnessPolicy,
) -> Result<DirectEvidence, Error> {
    if !policy.valid()
        || transaction.input.is_empty()
        || transaction.output.is_empty()
        || transaction.total_size() > 4_000_000
    {
        return Err(Error::InvalidRequest);
    }
    let mut generation = context.generation;
    let expected = context.expected_generation;
    if generation.has_changed().is_err() || *generation.borrow() != expected {
        return Err(Error::Cancelled);
    }
    let observed_at = super::now();
    let operation = async {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(DEADLINE)
            .build()
            .map_err(|_| Error::Transport)?;
        let auth = credentials(&config).await?;
        let before: BlockHash = rpc(
            &client,
            config.addr,
            &auth,
            1,
            "getbestblockhash",
            json!([]),
        )
        .await?;
        if before != expected_tip {
            return Err(Error::Stale);
        }
        let rows: Vec<Acceptance> = rpc(
            &client,
            config.addr,
            &auth,
            2,
            "testmempoolaccept",
            json!([[hex::encode(consensus::serialize(&transaction))]]),
        )
        .await?;
        let node_policy = acceptance(rows, &transaction)?;
        let after: BlockHash = rpc(
            &client,
            config.addr,
            &auth,
            3,
            "getbestblockhash",
            json!([]),
        )
        .await?;
        if after != before || !policy.fresh(observed_at, super::now()) {
            return Err(Error::Stale);
        }
        Ok(DirectEvidence {
            node: config.addr,
            txid: transaction.compute_txid(),
            wtxid: transaction.compute_wtxid(),
            tip: before,
            generation: expected,
            observed_at,
            policy: node_policy,
        })
    };
    let cancelled = async {
        loop {
            if generation.changed().await.is_err() || *generation.borrow_and_update() != expected {
                break;
            }
        }
    };
    let result = tokio::select! { biased;
        _ = cancelled => Err(Error::Cancelled),
        result = tokio::time::timeout(DEADLINE, operation) => result.map_err(|_| Error::Deadline)?,
    };
    if generation.has_changed().is_err() || *generation.borrow() != expected {
        return Err(Error::Cancelled);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::{self, hashes::Hash};
    use httpmock::prelude::*;
    use tokio::sync::watch;

    #[tokio::test]
    async fn direct_preflight_binds_witness_tip_and_policy_without_broadcast() {
        for case in 0..5 {
            let server = MockServer::start();
            let tx = Transaction {
                version: bitcoin::transaction::Version::TWO,
                lock_time: bitcoin::absolute::LockTime::ZERO,
                input: vec![bitcoin::TxIn::default()],
                output: vec![bitcoin::TxOut {
                    value: bitcoin::Amount::from_sat(1000),
                    script_pubkey: bitcoin::ScriptBuf::new(),
                }],
            };
            let tip = BlockHash::from_byte_array([1; 32]);
            let before = server.mock(|when, then| {
                when.method(POST).json_body(
                    json!({"jsonrpc":"2.0","id":1,"method":"getbestblockhash","params":[]}),
                );
                then.status(200)
                    .json_body(json!({"id":1,"result":tip,"error":null}));
            });
            let preflight = server.mock(|when, then| {
                when.method(POST).json_body(json!({"jsonrpc":"2.0","id":2,"method":"testmempoolaccept","params":[[hex::encode(consensus::serialize(&tx))]]}));
                let mut row = json!({"txid":tx.compute_txid(),"wtxid":tx.compute_wtxid(),"allowed":case != 1});
                if case == 1 { row["reject-reason"] = json!("scriptpubkey"); }
                if case == 2 { row["wtxid"] = json!("22".repeat(32)); }
                then.status(if case == 4 { 503 } else { 200 }).json_body(json!({"id":2,"result":[row],"error":null}));
            });
            let after = server.mock(|when, then| {
                when.method(POST).json_body(json!({"jsonrpc":"2.0","id":3,"method":"getbestblockhash","params":[]}));
                then.status(200).json_body(json!({"id":3,"result":if case == 3 { BlockHash::from_byte_array([2; 32]) } else { tip },"error":null}));
            });
            let config = BitcoindConfig {
                addr: *server.address(),
                rpc_auth: BitcoindRpcAuth::UserPass("synthetic".into(), "fixture".into()),
            };
            let (_live, generation) = watch::channel(3);
            let result = observe(
                config,
                tx.clone(),
                tip,
                CollectionContext {
                    expected_generation: 3,
                    generation,
                },
                FreshnessPolicy {
                    max_age_seconds: 30,
                    max_future_skew_seconds: 1,
                },
            )
            .await;
            before.assert_hits(1);
            preflight.assert_hits(1);
            match case {
                0 | 1 => {
                    let evidence = result.unwrap();
                    assert_eq!(evidence.txid(), tx.compute_txid());
                    assert_eq!(evidence.wtxid(), tx.compute_wtxid());
                    assert_eq!(evidence.tip(), tip);
                    assert_eq!(evidence.generation(), 3);
                    assert_eq!(
                        evidence.node_policy(),
                        &if case == 0 {
                            NodePolicy::Accepted
                        } else {
                            NodePolicy::Rejected {
                                reason: "scriptpubkey".into(),
                            }
                        }
                    );
                    after.assert_hits(1);
                }
                2 => {
                    assert_eq!(result, Err(Error::InvalidResponse));
                    after.assert_hits(0);
                }
                3 => assert_eq!(result, Err(Error::Stale)),
                4 => {
                    assert!(matches!(result, Err(Error::Http { status: 503, .. })));
                    after.assert_hits(0);
                }
                _ => unreachable!(),
            }
        }
    }
    #[tokio::test]
    async fn direct_preflight_cancels_active_io_promptly_without_followup_rpc() {
        let server = MockServer::start();
        let first = server.mock(|when, then| {
            when.method(POST)
                .json_body(json!({"jsonrpc":"2.0","id":1,"method":"getbestblockhash","params":[]}));
            then.status(200)
                .delay(std::time::Duration::from_secs(10))
                .json_body(json!({"id":1,"result":"11".repeat(32),"error":null}));
        });
        let unexpected = server.mock(|when, then| {
            when.method(POST).path("/");
            then.status(500);
        });
        let tx = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: bitcoin::absolute::LockTime::ZERO,
            input: vec![bitcoin::TxIn::default()],
            output: vec![bitcoin::TxOut {
                value: bitcoin::Amount::from_sat(1),
                script_pubkey: bitcoin::ScriptBuf::new(),
            }],
        };
        let config = BitcoindConfig {
            addr: *server.address(),
            rpc_auth: BitcoindRpcAuth::UserPass("synthetic".into(), "fixture".into()),
        };
        let (live, generation) = watch::channel(4);
        let task = tokio::spawn(observe(
            config,
            tx,
            BlockHash::from_byte_array([0x11; 32]),
            CollectionContext {
                expected_generation: 4,
                generation,
            },
            FreshnessPolicy {
                max_age_seconds: 30,
                max_future_skew_seconds: 1,
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while first.hits() == 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        live.send_replace(5);
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result, Err(Error::Cancelled));
        first.assert_hits(1);
        unexpected.assert_hits(0);
    }
    #[tokio::test]
    async fn direct_rpc_rejects_bad_ids_errors_oversized_bodies_and_redirects() {
        for case in 0..4 {
            let server = MockServer::start();
            let response = server.mock(|when, then| {
                when.method(POST).path("/");
                match case {
                    0 => { then.status(200).json_body(json!({"id":2,"result":"11".repeat(32),"error":null})); }
                    1 => { then.status(200).json_body(json!({"id":1,"result":"11".repeat(32),"error":{"code":-28,"message":"warming up"}})); }
                    2 => { then.status(200).body(" ".repeat(MAX_RESPONSE_BYTES + 1)); }
                    _ => { then.status(302).header("Location", format!("{}/redirect", server.base_url())); }
                }
            });
            let redirect = server.mock(|when, then| {
                when.path("/redirect");
                then.status(200);
            });
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
            let result: Result<BlockHash, Error> = rpc(
                &client,
                *server.address(),
                &("synthetic".into(), "fixture".into()),
                1,
                "getbestblockhash",
                json!([]),
            )
            .await;
            match case {
                0 | 1 => assert_eq!(result, Err(Error::InvalidResponse)),
                2 => assert_eq!(result, Err(Error::ResponseTooLarge)),
                _ => assert!(matches!(result, Err(Error::Http { status: 302, .. }))),
            }
            response.assert_hits(1);
            redirect.assert_hits(0);
        }
    }
}
