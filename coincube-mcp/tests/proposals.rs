#![cfg(any(unix, windows))]
use coincube_mcp::{ipc::*, proposal::*, *};
use std::{sync::Arc, time::Duration};

fn input() -> PrepareInput {
    PrepareInput {
        request_id: "regtest-demo-1".into(),
        topology: Topology {
            primary: Quorum {
                threshold: 3,
                key_count: 5,
            },
            recovery: Recovery {
                quorum: Quorum {
                    threshold: 1,
                    key_count: 1,
                },
                relative_delay_blocks: 144,
            },
        },
    }
}
fn proposal(response: Response) -> Proposal {
    match response {
        Response::Proposal(p) => p,
        _ => panic!("unexpected response"),
    }
}
fn server(store: Arc<Store>) -> Server {
    Server::start(
        store,
        Grant::for_proposals(Duration::from_secs(60), || true),
    )
    .unwrap()
}

#[test]
fn bounded_drafts_are_retry_safe_and_never_descriptors() {
    let store = Store::default();
    let first = store.prepare(input()).unwrap();
    assert_eq!(first.state, ProposalState::NeedsKeys);
    assert!(!first.descriptor_validated);
    assert_eq!(first.network, "regtest");
    assert_eq!(first.revision, 1);
    assert_eq!(store.prepare(input()).unwrap(), first);
    let mut changed = input();
    changed.topology.recovery.relative_delay_blocks = 145;
    assert_eq!(store.prepare(changed), Err(Error::ProposalConflict));
    let mut another = input();
    another.request_id = "another-request".into();
    assert_eq!(store.prepare(another), Err(Error::ProposalConflict));
    assert_eq!(
        store
            .get(&ProposalInput {
                proposal_id: first.proposal_id.clone()
            })
            .unwrap(),
        first
    );
    assert_eq!(
        store.get(&ProposalInput {
            proposal_id: "foreign".into()
        }),
        Err(Error::AccessDenied)
    );
    let json = serde_json::to_value(&first).unwrap();
    let expected = [
        "proposal_id",
        "revision",
        "network",
        "script_type",
        "topology",
        "state",
        "descriptor_validated",
    ];
    assert_eq!(json.as_object().unwrap().len(), expected.len());
    assert!(expected.iter().all(|key| json.get(key).is_some()));
    store.revoke();
    assert_eq!(store.prepare(input()), Err(Error::AccessDenied));
    assert_eq!(
        store.get(&ProposalInput {
            proposal_id: first.proposal_id
        }),
        Err(Error::AccessDenied)
    );
}

#[test]
fn invalid_topologies_and_references_do_not_fill_the_slot() {
    let store = Store::default();
    let mut cases = vec![];
    for count in [0, 4, 6, 255] {
        let mut v = input();
        v.topology.primary.key_count = count;
        cases.push(v);
    }
    for threshold in [0, 2, 4, 255] {
        let mut v = input();
        v.topology.primary.threshold = threshold;
        cases.push(v);
    }
    for (threshold, count) in [(0, 1), (2, 1), (1, 0), (1, 6)] {
        let mut v = input();
        v.topology.recovery.quorum = Quorum {
            threshold,
            key_count: count,
        };
        cases.push(v);
    }
    let mut zero_delay = input();
    zero_delay.topology.recovery.relative_delay_blocks = 0;
    cases.push(zero_delay);
    for id in [
        "".into(),
        "x".repeat(65),
        "file:///tmp/request".into(),
        "untrusted\ntext".into(),
    ] {
        let mut v = input();
        v.request_id = id;
        cases.push(v);
    }
    for v in cases {
        assert_eq!(store.prepare(v), Err(Error::InvalidInput));
    }
    let p = store.prepare(input()).unwrap();
    assert_eq!(
        store.get(&ProposalInput {
            proposal_id: "x".repeat(65)
        }),
        Err(Error::InvalidInput)
    );
    assert_eq!(p.topology, input().topology);
}

#[tokio::test]
async fn proposal_and_read_grants_are_disjoint_over_real_transport() {
    let store = Arc::new(Store::default());
    let proposal_server = server(store.clone());
    let p = proposal(
        call(
            &proposal_server.socket,
            proposal_server.grant.credential(),
            Request::PrepareProposal(input()),
        )
        .await
        .unwrap(),
    );
    for request in [
        Request::List(Empty {}),
        Request::Summary(WalletInput {
            wallet_id: "wallet".into(),
        }),
        Request::History(HistoryInput {
            wallet_id: "wallet".into(),
            limit: 1,
        }),
        Request::Policy(WalletInput {
            wallet_id: "wallet".into(),
        }),
    ] {
        assert_eq!(
            call(
                &proposal_server.socket,
                proposal_server.grant.credential(),
                request
            )
            .await
            .unwrap_err(),
            Error::AccessDenied
        );
    }
    let reader_server = Server::start(
        store,
        Grant::new("wallet".into(), [true; 3], Duration::from_secs(60)),
    )
    .unwrap();
    for request in [
        Request::PrepareProposal(input()),
        Request::Proposal(ProposalInput {
            proposal_id: p.proposal_id.clone(),
        }),
    ] {
        assert_eq!(
            call(
                &reader_server.socket,
                reader_server.grant.credential(),
                request
            )
            .await
            .unwrap_err(),
            Error::AccessDenied
        );
    }
    let other = server(Arc::new(Store::default()));
    assert_eq!(
        call(
            &other.socket,
            proposal_server.grant.credential(),
            Request::PrepareProposal(input())
        )
        .await
        .unwrap_err(),
        Error::AccessDenied
    );
    assert_eq!(
        call(
            &other.socket,
            other.grant.credential(),
            Request::Proposal(ProposalInput {
                proposal_id: p.proposal_id
            })
        )
        .await
        .unwrap_err(),
        Error::AccessDenied
    );
    proposal_server.grant.revoke();
    assert_eq!(
        call(
            &proposal_server.socket,
            proposal_server.grant.credential(),
            Request::PrepareProposal(input())
        )
        .await
        .unwrap_err(),
        Error::AccessDenied
    );
}

#[test]
fn concurrent_retries_still_have_one_bounded_draft_and_revocation_is_final() {
    let store = Arc::new(Store::default());
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.prepare(input()).unwrap()
            })
        })
        .collect();
    let proposals: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(proposals.iter().all(|p| p == &proposals[0]));
    store.revoke();
    assert_eq!(store.prepare(input()), Err(Error::AccessDenied));
}

#[cfg(feature = "adapter")]
#[tokio::test]
async fn sdk_proposals_are_state_changing_strict_and_do_not_upgrade_read_access() {
    use coincube_mcp::adapter::{Adapter, tools};
    let server = server(Arc::new(Store::default()));
    let adapter = Adapter {
        socket: server.socket.clone(),
        credential: server.grant.credential().into(),
    };
    let tool = tools()
        .into_iter()
        .find(|t| t.name == "tenshu_prepare_wallet_proposal")
        .unwrap();
    let annotations = tool.annotations.unwrap();
    assert_eq!(annotations.read_only_hint, Some(false));
    assert_eq!(annotations.idempotent_hint, Some(true));
    for field in [
        "xpub",
        "descriptor",
        "network",
        "approve",
        "credential",
        "path",
    ] {
        let mut v = serde_json::to_value(input()).unwrap();
        v[field] = serde_json::json!("untrusted");
        assert!(
            adapter
                .invoke("tenshu_prepare_wallet_proposal", v)
                .await
                .is_err()
        );
    }
    let result = adapter
        .invoke(
            "tenshu_prepare_wallet_proposal",
            serde_json::to_value(input()).unwrap(),
        )
        .await
        .unwrap();
    let json = serde_json::to_value(result).unwrap();
    assert_eq!(json["structuredContent"]["state"], "needs_keys");
    assert_eq!(json["structuredContent"]["descriptor_validated"], false);
    let result = adapter
        .invoke("tenshu_list_wallets", serde_json::json!({}))
        .await
        .unwrap();
    let json = serde_json::to_value(result).unwrap();
    assert_eq!(
        json["structuredContent"],
        serde_json::json!({"error":{"code":"access_denied"}})
    );
}

#[cfg(feature = "adapter")]
#[tokio::test]
async fn actual_stdio_proposal_is_not_a_wallet_and_cannot_approve_itself() {
    use serde_json::{Value, json};
    use std::process::Stdio;
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        process::Command,
    };
    let server = server(Arc::new(Store::default()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_tenshu-mcp"))
        .env("TENSHU_MCP_SOCKET", &server.socket)
        .env("TENSHU_MCP_CREDENTIAL", server.grant.credential())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    async fn exchange(
        stdin: &mut tokio::process::ChildStdin,
        lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
        id: u32,
        method: &str,
        params: Value,
    ) -> Value {
        let mut bytes =
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                .unwrap();
        bytes.push(b'\n');
        stdin.write_all(&bytes).await.unwrap();
        let response = tokio::time::timeout(Duration::from_secs(3), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let value: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(value["id"], id);
        value
    }
    let initialized = exchange(&mut stdin, &mut lines, 1, "initialize", json!({
        "protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"proposal-fixture","version":"1"}
    })).await;
    assert_eq!(initialized["result"]["serverInfo"]["name"], "tenshu-mcp");
    stdin
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    let prepared = exchange(
        &mut stdin,
        &mut lines,
        2,
        "tools/call",
        json!({
            "name":"tenshu_prepare_wallet_proposal", "arguments":input()
        }),
    )
    .await;
    let draft = &prepared["result"]["structuredContent"];
    assert_eq!(draft["state"], "needs_keys");
    assert_eq!(draft["descriptor_validated"], false);
    assert_eq!(
        draft["topology"]["primary"],
        json!({"threshold":3,"key_count":5})
    );
    assert!(!prepared.to_string().contains(server.grant.credential()));
    let retried = exchange(
        &mut stdin,
        &mut lines,
        3,
        "tools/call",
        json!({
            "name":"tenshu_prepare_wallet_proposal", "arguments":input()
        }),
    )
    .await;
    assert_eq!(retried["result"]["structuredContent"], *draft);
    let status = exchange(
        &mut stdin,
        &mut lines,
        4,
        "tools/call",
        json!({
            "name":"tenshu_get_wallet_proposal", "arguments":{"proposal_id":draft["proposal_id"]}
        }),
    )
    .await;
    assert_eq!(status["result"]["structuredContent"], *draft);
    let denied = exchange(
        &mut stdin,
        &mut lines,
        5,
        "tools/call",
        json!({
            "name":"tenshu_list_wallets", "arguments":{}
        }),
    )
    .await;
    assert_eq!(
        denied["result"]["structuredContent"],
        json!({"error":{"code":"access_denied"}})
    );
    let unavailable = exchange(
        &mut stdin,
        &mut lines,
        6,
        "tools/call",
        json!({
            "name":"tenshu_create_wallet", "arguments":{"proposal_id":draft["proposal_id"]}
        }),
    )
    .await;
    assert_eq!(unavailable["error"]["code"], -32602);
    server.grant.revoke();
    let revoked = exchange(
        &mut stdin,
        &mut lines,
        7,
        "tools/call",
        json!({
            "name":"tenshu_get_wallet_proposal", "arguments":{"proposal_id":draft["proposal_id"]}
        }),
    )
    .await;
    assert_eq!(
        revoked["result"]["structuredContent"],
        json!({"error":{"code":"access_denied"}})
    );
    drop(stdin);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert!(lines.next_line().await.unwrap().is_none());
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .await
        .unwrap();
    assert!(stderr.is_empty());
}

#[test]
fn native_admission_is_one_shot_and_serialized_with_revoke() {
    use std::{
        sync::{
            Arc, Barrier,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };
    for revoke_first in [false, true] {
        let grant = Arc::new(coincube_mcp::ipc::Grant::for_proposals(
            std::time::Duration::from_secs(60),
            || true,
        ));
        if revoke_first {
            grant.revoke();
        }
        let admitted = AtomicUsize::new(0);
        let result = grant.admit_native(|| {
            admitted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        assert_eq!(result.is_ok(), !revoke_first);
        assert_eq!(admitted.load(Ordering::SeqCst), usize::from(!revoke_first));
        assert!(!grant.active());
        assert!(grant.admit_native(|| Ok(())).is_err());
    }
    // Failed native checks leave no authority consumed or work scheduled.
    let grant =
        coincube_mcp::ipc::Grant::for_proposals(std::time::Duration::from_secs(60), || true);
    assert!(
        grant
            .admit_native(|| Err::<(), _>(coincube_mcp::Error::AccessDenied))
            .is_err()
    );
    assert!(grant.active());
    // The racing revoke/admit have a single winner. No second admission.
    for _ in 0..32 {
        let grant = Arc::new(coincube_mcp::ipc::Grant::for_proposals(
            std::time::Duration::from_secs(60),
            || true,
        ));
        let start = Arc::new(Barrier::new(2));
        let other = grant.clone();
        let barrier = start.clone();
        let worker = thread::spawn(move || {
            barrier.wait();
            other.revoke();
        });
        start.wait();
        let _ = grant.admit_native(|| Ok(()));
        worker.join().unwrap();
        assert!(!grant.active());
        assert!(grant.admit_native(|| Ok(())).is_err());
    }
    let read = coincube_mcp::ipc::Grant::new(
        "fixture".into(),
        [true; 3],
        std::time::Duration::from_secs(60),
    );
    assert!(read.admit_native(|| Ok(())).is_err());
}

#[test]
fn native_validation_cannot_extend_expiration() {
    let grant =
        coincube_mcp::ipc::Grant::for_proposals(std::time::Duration::from_millis(20), || true);
    assert!(
        grant
            .admit_native(|| {
                std::thread::sleep(std::time::Duration::from_millis(40));
                Ok(())
            })
            .is_err()
    );
    assert!(!grant.active());
}
