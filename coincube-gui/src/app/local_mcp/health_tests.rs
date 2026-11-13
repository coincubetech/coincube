use super::*;
use crate::daemon::{
    client::{Client, Coincubed},
    model::GetInfoResult,
    DaemonError,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::json;
use std::{
    fmt::Debug,
    sync::atomic::{AtomicUsize, Ordering},
};

/// The real read projection with deterministic daemon health changes during a
/// data query. All responses are synthetic; no external daemon is contacted.
#[derive(Debug)]
struct HealthClient {
    before: GetInfoResult,
    after: GetInfoResult,
    data_reads: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl Client for HealthClient {
    type Error = DaemonError;
    async fn request<S: Serialize + Debug + Send, D: DeserializeOwned + Debug + Send>(
        &self,
        method: &str,
        _: Option<S>,
    ) -> Result<D, Self::Error> {
        let value = match method {
            "getinfo" => serde_json::to_value(if self.data_reads.load(Ordering::SeqCst) == 0 {
                &self.before
            } else {
                &self.after
            })
            .unwrap(),
            "listcoins" => {
                self.data_reads.fetch_add(1, Ordering::SeqCst);
                json!({"coins":[]})
            }
            "listconfirmed" => {
                self.data_reads.fetch_add(1, Ordering::SeqCst);
                json!({"transactions":[]})
            }
            "getlabels" => json!({"labels":{}}),
            other => panic!("unexpected health fixture call: {}", other),
        };
        Ok(serde_json::from_value(value).unwrap())
    }
}

fn reader(before: GetInfoResult, after: GetInfoResult) -> (VaultReader, Arc<AtomicUsize>) {
    let data_reads = Arc::new(AtomicUsize::new(0));
    (
        VaultReader {
            descriptor: before.descriptors.main.as_vault().unwrap().clone(),
            chain: before.chain_id.unwrap(),
            daemon: Arc::new(Coincubed::new(HealthClient {
                before,
                after,
                data_reads: data_reads.clone(),
            })),
            wallet_id: "health-wallet".into(),
            read_slots: Arc::new(tokio::sync::Semaphore::new(2)),
            activity_key: [42; 32],
            last_poll_at_startup: Some(1),
        },
        data_reads,
    )
}

async fn healthy_info() -> GetInfoResult {
    let (root, wallet, daemon) = super::tests::fixture();
    let mut info = daemon.get_info().await.unwrap();
    daemon.stop().await.unwrap();
    drop(daemon);
    drop(wallet);
    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(root))
        .await
        .unwrap()
        .unwrap();
    info.sync = 1.0;
    info.block_height = 10;
    info.last_poll_timestamp = Some(2);
    info.history_sync = Default::default();
    info
}

fn summary() -> wire::Request {
    wire::Request::Summary(wire::WalletInput {
        wallet_id: "health-wallet".into(),
    })
}
fn history() -> wire::Request {
    wire::Request::History(wire::HistoryInput {
        wallet_id: "health-wallet".into(),
        limit: 10,
    })
}

#[tokio::test]
async fn mcp_read_health_covers_rescans_chain_alerts_and_incomplete_sync() {
    let healthy = healthy_info().await;
    let (r, _) = reader(healthy.clone(), healthy.clone());
    assert_eq!(r.sync_state(&healthy), "synced");
    let mut cases = Vec::new();
    for progress in [0.0, 0.5, 1.0] {
        let mut info = healthy.clone();
        info.rescan_progress = Some(progress);
        cases.push((info, "syncing"));
    }
    let mut info = healthy.clone();
    info.refused_reorg_depth = Some(100);
    cases.push((info, "sync_failing"));
    let mut info = healthy.clone();
    info.chain_divergence = true;
    cases.push((info, "sync_failing"));
    let mut info = healthy.clone();
    info.sync = 0.5;
    cases.push((info, "syncing"));
    let mut info = healthy.clone();
    info.history_sync.full_scan_in_progress = true;
    cases.push((info, "syncing"));
    let mut info = healthy.clone();
    info.last_poll_timestamp = Some(1);
    cases.push((info, "syncing"));
    let mut info = healthy.clone();
    info.history_sync.last_failure = Some(
        serde_json::from_value(json!({
            "message":"private backend diagnostics", "since":1, "last_at":2, "consecutive":2
        }))
        .unwrap(),
    );
    cases.push((info, "sync_failing"));

    for (unhealthy, expected) in cases {
        // Changes either before or during the query cannot be called synced.
        for (before, after) in [
            (unhealthy.clone(), unhealthy.clone()),
            (healthy.clone(), unhealthy.clone()),
            (unhealthy.clone(), healthy.clone()),
        ] {
            let (r, _) = reader(before.clone(), after.clone());
            let wire::Response::Summary(result) = r.read_wallet(summary()).await.unwrap() else {
                panic!("expected summary")
            };
            assert_eq!(result.sync_state, expected);
            assert!(!serde_json::to_string(&result)
                .unwrap()
                .contains("private backend"));
            let (r, reads) = reader(before, after);
            let initially_healthy = r.sync_state(&r.wallet_info().await.unwrap()) == "synced";
            assert_eq!(
                r.read_wallet(history()).await.unwrap_err(),
                wire::Error::Unavailable
            );
            // The initial unhealthy snapshot should stop before the data query.
            // During-read changes are checked again before releasing results.
            assert_eq!(
                reads.load(Ordering::SeqCst),
                if initially_healthy { 2 } else { 0 }
            );
        }
    }
    for progress in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1, 1.1] {
        let mut info = healthy.clone();
        info.sync = progress;
        assert_eq!(r.sync_state(&info), "sync_failing");
    }
    let (r, _) = reader(healthy.clone(), healthy);
    let wire::Response::History(result) = r.read_wallet(history()).await.unwrap() else {
        panic!("expected healthy empty history")
    };
    assert!(result.transactions.is_empty() && result.confirmed_only);
}

#[tokio::test]
async fn mcp_read_health_rechecks_wallet_identity_after_data_query() {
    let healthy = healthy_info().await;
    let mut replacement = healthy.clone();
    replacement.chain_id = None;
    for request in [summary(), history()] {
        let (r, _) = reader(healthy.clone(), replacement.clone());
        assert_eq!(
            r.read_wallet(request).await.unwrap_err(),
            wire::Error::Unavailable
        );
    }
}
