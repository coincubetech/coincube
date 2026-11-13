#![cfg(all(feature = "adapter", any(unix, windows)))]

use coincube_mcp::{adapter::Adapter, ipc::*, *};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct RecordingReader(AtomicUsize);
impl Reader for RecordingReader {
    fn read(
        &self,
        _: Request,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Response, Error>> + Send + '_>>
    {
        Box::pin(async {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(Error::Unavailable)
        })
    }
}

/// Use valid arguments AND a matching grant: disabled operations must stop at
/// the adapter, rather than appearing safe only because the desktop denies them.
#[tokio::test]
async fn dispatch_matches_discovery_and_capabilities() {
    let topology = json!({"primary":{"threshold":3,"key_count":5},
        "recovery":{"quorum":{"threshold":1,"key_count":1},"relative_delay_blocks":144}});
    let caps = Capabilities::current();
    let credential = "a".repeat(64);
    for (name, enabled, request, grant) in [
        (
            "tenshu_prepare_wallet_proposal",
            caps.wallet_proposals,
            json!({"request_id":"request-1","network":"regtest","topology":topology}),
            Grant::paired_proposals(credential.clone(), || true).unwrap(),
        ),
        (
            "tenshu_get_wallet_proposal",
            caps.wallet_proposals,
            json!({"proposal_id":"proposal-1"}),
            Grant::paired_proposals(credential.clone(), || true).unwrap(),
        ),
        (
            "tenshu_prepare_wallet_creation",
            caps.wallet_proposals,
            json!({"request_id":"request-1","network":"regtest","topology":topology,
                "keys":{"primary":["public-1","public-2","public-3","public-4","public-5"],"recovery":["public-6"]}}),
            Grant::paired_creation(credential.clone(), || true).unwrap(),
        ),
        (
            "tenshu_prepare_transaction",
            caps.transaction_preparation,
            json!({"wallet_id":"wallet","request_id":"request-1","network":"regtest",
                "recipients":[{"address":"bcrt1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqdku202","amount_sats":10000}],
                "fee_rate_sat_vb":2,"max_fee_sats":1000}),
            Grant::paired_transactions("wallet".into(), credential.clone(), || true).unwrap(),
        ),
        (
            "tenshu_get_prepared_transaction",
            caps.transaction_preparation,
            json!({"wallet_id":"wallet","draft_id":"draft-1"}),
            Grant::paired_transactions("wallet".into(), credential.clone(), || true).unwrap(),
        ),
    ] {
        assert_eq!(
            adapter::tools().iter().any(|tool| tool.name == name),
            enabled
        );
        let reader = Arc::new(RecordingReader::default());
        let server = Server::start(reader.clone(), grant).unwrap();
        let adapter = Adapter {
            socket: server.socket.clone(),
            credential: credential.clone(),
            pairing: None,
        };
        let result = adapter.invoke(name, request).await;
        if enabled {
            let result = serde_json::to_value(result.unwrap()).unwrap();
            assert_eq!(
                result["structuredContent"]["error"]["code"],
                "wallet_unavailable"
            );
            assert_eq!(reader.0.load(Ordering::SeqCst), 1, "{name}");
        } else {
            assert_eq!(result.unwrap_err().message, "unknown_tool", "{name}");
            assert_eq!(reader.0.load(Ordering::SeqCst), 0, "{name}");
        }
    }
}
