//! Preflight-selected routes. A route is review data, never send authority.
use super::*;
use crate::services::claim_preflight::direct::{self, DirectEvidence};
use coincubed::config::{BitcoindConfig, BitcoindRpcAuth};
use std::net::SocketAddr;

/// Process-local binding; contains no credential digest or persisted identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeIdentity(u64);
#[derive(Clone)]
pub(super) struct BoundNode {
    identity: NodeIdentity,
    config: BitcoindConfig,
}
impl BoundNode {
    pub(super) fn new(config: BitcoindConfig) -> Self {
        static NEXT_NODE: AtomicU64 = AtomicU64::new(1);
        Self {
            identity: NodeIdentity(NEXT_NODE.fetch_add(1, Ordering::Relaxed)),
            config,
        }
    }
    pub(super) fn matches(&self, config: &BitcoindConfig) -> bool {
        self.config.addr == config.addr
            && match (&self.config.rpc_auth, &config.rpc_auth) {
                (BitcoindRpcAuth::CookieFile(a), BitcoindRpcAuth::CookieFile(b)) => a == b,
                (BitcoindRpcAuth::UserPass(au, ap), BitcoindRpcAuth::UserPass(bu, bp)) => {
                    au == bu && ap == bp
                }
                _ => false,
            }
    }
    pub(super) fn route(&self) -> SubmissionRoute {
        SubmissionRoute::BitcoinNode {
            address: self.config.addr,
            identity: self.identity,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionRoute {
    Connect,
    BitcoinNode {
        address: SocketAddr,
        identity: NodeIdentity,
    },
}
impl SubmissionRoute {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connect => "Connect",
            Self::BitcoinNode { .. } => "Your Bitcoin node",
        }
    }
}

pub(super) enum RoutedEvidence {
    Connect(Evidence),
    BitcoinNode(DirectEvidence, NodeIdentity),
}
impl RoutedEvidence {
    pub(super) fn route(&self) -> SubmissionRoute {
        match self {
            Self::Connect(_) => SubmissionRoute::Connect,
            Self::BitcoinNode(e, identity) => SubmissionRoute::BitcoinNode {
                address: e.node(),
                identity: *identity,
            },
        }
    }
    pub(super) fn chain(&self) -> ChainId {
        match self {
            Self::Connect(e) => e.chain(),
            Self::BitcoinNode(_, _) => ChainId::Bitcoin,
        }
    }
    pub(super) fn txid(&self) -> Txid {
        match self {
            Self::Connect(e) => e.txid(),
            Self::BitcoinNode(e, _) => e.txid(),
        }
    }
    pub(super) fn wtxid(&self) -> Wtxid {
        match self {
            Self::Connect(e) => e.wtxid(),
            Self::BitcoinNode(e, _) => e.wtxid(),
        }
    }
    pub(super) fn tip(&self) -> BlockHash {
        match self {
            Self::Connect(e) => e.tip(),
            Self::BitcoinNode(e, _) => e.tip(),
        }
    }
    pub(super) fn generation(&self) -> u64 {
        match self {
            Self::Connect(e) => e.generation(),
            Self::BitcoinNode(e, _) => e.generation(),
        }
    }
    pub(super) fn observed_at(&self) -> i64 {
        match self {
            Self::Connect(e) => e.observed_at(),
            Self::BitcoinNode(e, _) => e.observed_at(),
        }
    }
    pub(super) fn node_policy(&self) -> &NodePolicy {
        match self {
            Self::Connect(e) => e.node_policy(),
            Self::BitcoinNode(e, _) => e.node_policy(),
        }
    }
}

/// Only a definite local policy rejection selects the operator fallback. Local
/// transport failure, changed tip, malformed data or cancellation stays an error.
pub(super) async fn preflight(
    local: Option<BoundNode>,
    operator: &PreflightClient,
    tx: &Transaction,
    tip: BlockHash,
    policy: FreshnessPolicy,
    context: CollectionContext,
) -> Result<RoutedEvidence, claim_preflight::Error> {
    let local_rejection = if let Some(bound) = local {
        let evidence = direct::observe(bound.config, tx.clone(), tip, context, policy).await?;
        match evidence.node_policy() {
            NodePolicy::Accepted => {
                return Ok(RoutedEvidence::BitcoinNode(evidence, bound.identity))
            }
            NodePolicy::Rejected { reason } => Some(reason.clone()),
        }
    } else {
        None
    };
    let evidence = operator.observe(ChainId::Bitcoin, tx, tip, policy).await?;
    if let (Some(local), NodePolicy::Rejected { reason }) =
        (local_rejection, evidence.node_policy())
    {
        return Err(claim_preflight::Error::BothRoutesRejected {
            local,
            connect: reason.clone(),
        });
    }
    Ok(RoutedEvidence::Connect(evidence))
}

/// Local-node policy only, with no operator fallback: Split step 2 on a
/// BTCB2 Vault's managed Knots node (#568 P4). The node's best block must
/// equal `tip`, the independently observed BTCB2 tip, so a node on any other
/// chain refuses as `Stale`. A rejection is returned as evidence; the caller
/// refuses it.
pub(super) async fn node_preflight(
    bound: &BoundNode,
    tx: &Transaction,
    tip: BlockHash,
    policy: FreshnessPolicy,
    context: CollectionContext,
) -> Result<RoutedEvidence, claim_preflight::Error> {
    let evidence = direct::observe(bound.config.clone(), tx.clone(), tip, context, policy).await?;
    Ok(RoutedEvidence::BitcoinNode(evidence, bound.identity))
}

#[cfg(test)]
mod tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::{
        self, absolute, transaction, Amount, ScriptBuf, TxIn, TxOut,
    };
    use coincubed::config::BitcoindRpcAuth;
    use httpmock::prelude::*;
    use serde_json::json;
    #[tokio::test]
    async fn preflight_selects_local_or_checked_connect_and_preserves_both_rejections() {
        for case in 0..4 {
            let server = MockServer::start();
            let tx = Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn::default()],
                output: vec![TxOut {
                    value: Amount::from_sat(1),
                    script_pubkey: ScriptBuf::new(),
                }],
            };
            let tip = BlockHash::from_byte_array([1; 32]);
            for id in [1, 3] {
                server.mock(|when, then| {
                    when.method(POST).path("/").json_body(
                        json!({"jsonrpc":"2.0","id":id,"method":"getbestblockhash","params":[]}),
                    );
                    then.status(200)
                        .json_body(json!({"id":id,"result":tip,"error":null}));
                });
            }
            let local = server.mock(|when, then| {
                when.method(POST).path("/").json_body(json!({"jsonrpc":"2.0","id":2,"method":"testmempoolaccept","params":[[hex::encode(bitcoin::consensus::serialize(&tx))]]}));
                let mut row = json!({"txid":tx.compute_txid(),"wtxid":tx.compute_wtxid(),"allowed":case == 0});
                if case != 0 { row["reject-reason"] = json!("local-policy"); }
                then.status(if case == 3 { 503 } else { 200 }).json_body(json!({"id":2,"result":[row],"error":null}));
            });
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let operator = server.mock(|when, then| {
                when.method(POST).path("/api/v1/esplora/bitcoin/mainnet/tx/preflight");
                then.status(200).header("cache-control","no-store").json_body(json!({"success":true,"data":{"network":"mainnet","state":"available","result":{"txid":tx.compute_txid(),"wtxid":tx.compute_wtxid(),"tip_hash":tip,"observed_at":stamp,"allowed":case != 2,"reject_reason":if case == 2 { json!("operator-policy") } else { serde_json::Value::Null }}}}));
            });
            let (_live, generation) = watch::channel(7);
            let context = CollectionContext {
                expected_generation: 7,
                generation,
            };
            let client = PreflightClient::new(
                &server.base_url(),
                CollectionContext {
                    expected_generation: context.expected_generation,
                    generation: context.generation.clone(),
                },
            )
            .unwrap();
            let config = BitcoindConfig {
                addr: *server.address(),
                rpc_auth: BitcoindRpcAuth::UserPass("synthetic".into(), "fixture".into()),
            };
            let bound = BoundNode::new(config);
            let selected = bound.route();
            let result = preflight(
                Some(bound),
                &client,
                &tx,
                tip,
                FreshnessPolicy {
                    max_age_seconds: 30,
                    max_future_skew_seconds: 1,
                },
                context,
            )
            .await;
            local.assert_hits(1);
            match case {
                0 => {
                    assert_eq!(result.unwrap().route(), selected);
                    operator.assert_hits(0);
                }
                1 => {
                    assert_eq!(result.unwrap().route(), SubmissionRoute::Connect);
                    operator.assert_hits(1);
                }
                2 => assert!(
                    matches!(result, Err(claim_preflight::Error::BothRoutesRejected { local, connect }) if local == "local-policy" && connect == "operator-policy")
                ),
                _ => {
                    assert!(matches!(
                        result,
                        Err(claim_preflight::Error::Http { status: 503, .. })
                    ));
                    operator.assert_hits(0);
                }
            }
        }
    }
    #[test]
    fn backend_identity_changes_at_the_same_address_and_compares_credentials_privately() {
        let config = BitcoindConfig {
            addr: "127.0.0.1:18443".parse().unwrap(),
            rpc_auth: BitcoindRpcAuth::UserPass("synthetic-user".into(), "synthetic-secret".into()),
        };
        let first = BoundNode::new(config.clone());
        let replacement = BoundNode::new(config.clone());
        assert_ne!(first.route(), replacement.route());
        assert_eq!(first.route(), first.clone().route());
        assert!(first.matches(&config));
        let mut changed = config;
        changed.rpc_auth =
            BitcoindRpcAuth::UserPass("synthetic-user".into(), "changed-secret".into());
        assert!(!first.matches(&changed));
        changed.rpc_auth = BitcoindRpcAuth::CookieFile("/synthetic/cookie".into());
        assert!(!first.matches(&changed));
        let debug = format!("{:?}", first.route());
        assert!(!debug.contains("synthetic-user"));
        assert!(!debug.contains("synthetic-secret"));
    }
}
