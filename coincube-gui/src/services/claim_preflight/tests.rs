use super::*;
use coincube_core::miniscript::bitcoin::{
    absolute, hashes::Hash, transaction, Amount, OutPoint, ScriptBuf, Sequence, TxIn, TxOut,
    Witness,
};
use httpmock::prelude::*;
use serde_json::{json, Value};
fn tx() -> Transaction {
    Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: Txid::from_byte_array([1; 32]),
                vout: 0,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::MAX,
            witness: Witness::from_slice(&[vec![1, 2, 3]]),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(1000),
            script_pubkey: ScriptBuf::new(),
        }],
    }
}
fn paired_tx(marker: u8) -> Transaction {
    let mut transaction = tx();
    transaction.input.push(TxIn {
        previous_output: OutPoint {
            txid: Txid::from_byte_array([marker; 32]),
            vout: 0,
        },
        script_sig: ScriptBuf::new(),
        sequence: Sequence::MAX,
        witness: Witness::from_slice(&[vec![marker]]),
    });
    transaction
}
fn tip() -> BlockHash {
    BlockHash::from_byte_array([2; 32])
}
fn policy() -> FreshnessPolicy {
    FreshnessPolicy {
        max_age_seconds: 60,
        max_future_skew_seconds: 2,
    }
}
fn headers() -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(CACHE_CONTROL, "no-store".parse().unwrap());
    h
}
fn response(chain: ChainId, stamp: i64) -> Value {
    json!({"success":true,"error":null,"data":{"network":chain.api_str(),"state":"available","result":{"txid":tx().compute_txid(),"wtxid":tx().compute_wtxid(),"tip_hash":tip(),"observed_at":stamp,"allowed":true}}})
}
fn pair_response(chain: ChainId, transaction: &Transaction, allowed: bool) -> Value {
    let mut body = response(chain, now());
    body["data"]["result"]["txid"] = json!(transaction.compute_txid());
    body["data"]["result"]["wtxid"] = json!(transaction.compute_wtxid());
    body["data"]["result"]["allowed"] = json!(allowed);
    if !allowed {
        body["data"]["result"]["reject_reason"] = json!("policy_rejected");
    }
    body
}

#[test]
fn paired_attempt_rechecks_both_exact_witnesses_and_observation_age() {
    let bitcoin = paired_tx(3);
    let fork = paired_tx(4);
    let pair = accepted_pair_for_test(&bitcoin, &fork, tip(), 9, 100);
    assert!(pair.accepted_for(&bitcoin, &fork, 9, policy(), 100));
    assert!(!pair.accepted_for(&bitcoin, &fork, 10, policy(), 100));
    assert!(!pair.accepted_for(&bitcoin, &fork, 9, policy(), 161));
    let mut changed_witness = fork.clone();
    changed_witness.input[0].witness = Witness::from_slice(&[vec![9]]);
    assert_eq!(changed_witness.compute_txid(), fork.compute_txid());
    assert_ne!(changed_witness.compute_wtxid(), fork.compute_wtxid());
    assert!(!pair.accepted_for(&bitcoin, &changed_witness, 9, policy(), 100));
    let mut rejected = pair;
    rejected.fork.policy = NodePolicy::Rejected {
        reason: "rejected".into(),
    };
    assert!(!rejected.accepted_for(&bitcoin, &fork, 9, policy(), 100));
}

#[tokio::test]
async fn paired_preflight_requires_distinct_markers_and_both_policy_acceptances() {
    let bitcoin = paired_tx(3);
    let fork = paired_tx(4);
    assert!(paired_input_shape(&bitcoin, &fork));
    assert!(!paired_input_shape(&bitcoin, &bitcoin));
    let mut missing_shared = fork.clone();
    missing_shared.input[0].previous_output.txid = Txid::from_byte_array([5; 32]);
    assert!(!paired_input_shape(&bitcoin, &missing_shared));
    let mut duplicate = fork.clone();
    duplicate.input[1].previous_output = duplicate.input[0].previous_output;
    assert!(!paired_input_shape(&bitcoin, &duplicate));

    for fork_allowed in [true, false] {
        let server = MockServer::start();
        let (client, _sender) = client(&server);
        let bitcoin_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v1/esplora/bitcoin/mainnet/tx/preflight");
            then.status(200)
                .header("Cache-Control", "no-store")
                .json_body(pair_response(ChainId::Bitcoin, &bitcoin, true));
        });
        let fork_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v1/esplora/bitcoin-blake2b/mainnet/tx/preflight");
            then.status(200)
                .header("Cache-Control", "no-store")
                .json_body(pair_response(ChainId::BitcoinBlake2b, &fork, fork_allowed));
        });
        let observed = client
            .observe_ancestry_pair(&bitcoin, &fork, tip(), tip(), policy())
            .await;
        if fork_allowed {
            let observed = observed.unwrap();
            assert_eq!(observed.bitcoin().txid(), bitcoin.compute_txid());
            assert_eq!(observed.fork().wtxid(), fork.compute_wtxid());
        } else {
            assert_eq!(
                observed,
                Err(Error::PairedPolicyRejected {
                    chain: ChainId::BitcoinBlake2b,
                    reason: "policy_rejected".into(),
                })
            );
        }
        bitcoin_mock.assert_hits(1);
        fork_mock.assert_hits(1);
    }
}

#[tokio::test]
async fn post_bitcoin_preflight_reads_only_the_exact_fork_witness() {
    let bitcoin = paired_tx(3);
    let fork = paired_tx(4);
    for allowed in [true, false] {
        let server = MockServer::start();
        let (client, _sender) = client(&server);
        let bitcoin_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v1/esplora/bitcoin/mainnet/tx/preflight");
            then.status(500);
        });
        let fork_mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v1/esplora/bitcoin-blake2b/mainnet/tx/preflight")
                .json_body(json!({
                    "transaction": consensus::encode::serialize_hex(&fork),
                    "tip_hash": tip(),
                }));
            then.status(200)
                .header("Cache-Control", "no-store")
                .json_body(pair_response(ChainId::BitcoinBlake2b, &fork, allowed));
        });
        let observed = client
            .observe_ancestry_fork(&bitcoin, &fork, tip(), policy())
            .await;
        if allowed {
            let evidence = observed.unwrap();
            assert_eq!(evidence.wtxid(), fork.compute_wtxid());
            assert_eq!(evidence.chain(), ChainId::BitcoinBlake2b);
        } else {
            assert_eq!(
                observed,
                Err(Error::PairedPolicyRejected {
                    chain: ChainId::BitcoinBlake2b,
                    reason: "policy_rejected".into(),
                })
            );
        }
        fork_mock.assert_hits(1);
        bitcoin_mock.assert_hits(0);
    }

    let server = MockServer::start();
    let (client, _sender) = client(&server);
    assert_eq!(
        client
            .observe_ancestry_fork(&bitcoin, &bitcoin, tip(), policy())
            .await,
        Err(Error::InvalidRequest)
    );
}
fn parse_value(status: StatusCode, body: Value) -> Result<Evidence, Error> {
    parse(
        status,
        &headers(),
        &serde_json::to_vec(&body).unwrap(),
        ChainId::BitcoinBlake2b,
        &tx(),
        tip(),
        7,
        policy(),
        10000,
    )
}
fn client(server: &MockServer) -> (PreflightClient, watch::Sender<u64>) {
    let (sender, generation) = watch::channel(7);
    (
        PreflightClient::new(
            &server.base_url(),
            CollectionContext {
                expected_generation: 7,
                generation,
            },
        )
        .unwrap(),
        sender,
    )
}
#[test]
fn paired_submission_uses_the_same_connect_origin_as_its_proof_provider() {
    let server = MockServer::start();
    let (client, _sender) = client(&server);
    assert!(client.matches_provider(&format!(
        "bitcoin|{}/api/v1/esplora/bitcoin/mainnet",
        server.base_url()
    )));
    assert!(!client.matches_provider(&format!(
        "bitcoin|{}/api/v1/esplora/bitcoin-blake2b/mainnet",
        server.base_url()
    )));
    assert!(
        !client.matches_provider("bitcoin|https://another.example/api/v1/esplora/bitcoin/mainnet")
    );
    assert_eq!(client.submission_origin(), server.base_url());
}
#[test]
fn exact_bindings_and_explicit_clock_bounds_are_required() {
    for (stamp, accepted) in [
        (9940, true),
        (9939, false),
        (10000, true),
        (10002, true),
        (10003, false),
        (-1, false),
    ] {
        assert_eq!(
            parse_value(StatusCode::OK, response(ChainId::BitcoinBlake2b, stamp)).is_ok(),
            accepted
        );
    }
    for field in ["txid", "wtxid", "tip_hash"] {
        let mut body = response(ChainId::BitcoinBlake2b, 10000);
        body["data"]["result"][field] = json!("33".repeat(32));
        assert_eq!(
            parse_value(StatusCode::OK, body),
            Err(Error::InvalidResponse)
        );
    }
    assert_eq!(
        parse_value(StatusCode::OK, response(ChainId::Bitcoin, 10000)),
        Err(Error::InvalidResponse)
    );
    let evidence = parse_value(StatusCode::OK, response(ChainId::BitcoinBlake2b, 10002)).unwrap();
    assert_eq!(evidence.observed_at(), 10002);
    assert_eq!(evidence.generation(), 7);
    assert_eq!(evidence.node_policy(), &NodePolicy::Accepted);
    for (age, skew) in [(0, 2), (60, 0), (60, -1), (60, 6)] {
        assert!(!FreshnessPolicy {
            max_age_seconds: age,
            max_future_skew_seconds: skew
        }
        .valid());
    }
}
#[test]
fn rejection_is_evidence_but_arbitrary_diagnostics_are_refused() {
    let mut body = response(ChainId::BitcoinBlake2b, 10000);
    body["data"]["result"]["allowed"] = json!(false);
    body["data"]["result"]["reject_reason"] = json!("policy_rejected");
    assert_eq!(
        parse_value(StatusCode::OK, body.clone())
            .unwrap()
            .node_policy(),
        &NodePolicy::Rejected {
            reason: "policy_rejected".into()
        }
    );
    for reason in ["", "private diagnostic", "UPPERCASE"] {
        body["data"]["result"]["reject_reason"] = json!(reason);
        assert_eq!(
            parse_value(StatusCode::OK, body.clone()),
            Err(Error::InvalidResponse)
        );
    }
    body["data"]["result"]["reject_reason"] = json!("a".repeat(81));
    assert_eq!(
        parse_value(StatusCode::OK, body),
        Err(Error::InvalidResponse)
    );
    let mut contradictory = response(ChainId::BitcoinBlake2b, 10000);
    contradictory["data"]["result"]["reject_reason"] = json!("not_allowed");
    assert_eq!(
        parse_value(StatusCode::OK, contradictory),
        Err(Error::InvalidResponse)
    );
}
#[test]
fn service_states_capacity_and_http_failures_remain_distinct() {
    for state in [
        "not_configured",
        "configuration_error",
        "rpc_unavailable",
        "malformed",
        "wrong_chain",
        "syncing",
        "inconsistent_snapshot",
        "fork_absent",
        "fork_inactive",
        "rdts_absent",
        "rdts_unsupported",
        "fork_unverified",
    ] {
        let body = json!({"success":false,"error":{"code":"SERVICE_UNAVAILABLE"},"data":{"network":"bitcoin-blake2b","state":state}});
        assert_eq!(
            parse_value(StatusCode::SERVICE_UNAVAILABLE, body),
            Err(Error::Service {
                state: serde_json::from_value(json!(state)).unwrap(),
                retry_after_seconds: None
            })
        );
    }
    assert_eq!(
        parse_value(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"success":false,"error":{"code":"SERVICE_UNAVAILABLE"}})
        ),
        Err(Error::Http {
            status: 503,
            retry_after_seconds: None
        })
    );
    for status in [400, 401, 404, 429, 500] {
        assert_eq!(
            parse_value(StatusCode::from_u16(status).unwrap(), json!({})),
            Err(Error::Http {
                status,
                retry_after_seconds: None
            })
        );
    }
    for mut body in [
        response(ChainId::BitcoinBlake2b, 10000),
        json!({"success":false,"error":{"code":"OTHER"}}),
    ] {
        body["success"] = json!(false);
        assert_eq!(
            parse_value(StatusCode::SERVICE_UNAVAILABLE, body),
            Err(Error::InvalidResponse)
        );
    }
    let mut body = response(ChainId::BitcoinBlake2b, 10000);
    body["success"] = json!(false);
    assert_eq!(
        parse_value(StatusCode::OK, body),
        Err(Error::InvalidResponse)
    );
    let partial = json!({"success":false,"error":{"code":"SERVICE_UNAVAILABLE"},"data":{"network":"bitcoin-blake2b","state":"rpc_unavailable","result":response(ChainId::BitcoinBlake2b,10000)["data"]["result"]}});
    assert_eq!(
        parse_value(StatusCode::SERVICE_UNAVAILABLE, partial),
        Err(Error::InvalidResponse)
    );
}
#[test]
fn witness_mutation_is_detected_even_with_same_txid() {
    let mut changed = tx();
    changed.input[0].witness.push([4]);
    assert_eq!(changed.compute_txid(), tx().compute_txid());
    assert_ne!(changed.compute_wtxid(), tx().compute_wtxid());
    assert_eq!(
        parse(
            StatusCode::OK,
            &headers(),
            &serde_json::to_vec(&response(ChainId::BitcoinBlake2b, 10000)).unwrap(),
            ChainId::BitcoinBlake2b,
            &changed,
            tip(),
            7,
            policy(),
            10000
        ),
        Err(Error::InvalidResponse)
    );
}
#[tokio::test]
async fn actual_post_is_anonymous_fixed_route_and_exact_transaction() {
    for chain in [ChainId::Bitcoin, ChainId::BitcoinBlake2b] {
        let server = MockServer::start();
        let (client, _sender) = client(&server);
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path(format!(
                    "/api/v1/esplora/{}/tx/preflight",
                    route(chain).unwrap()
                ))
                .header("cache-control", "no-cache")
                .json_body(
                    json!({"transaction":consensus::encode::serialize_hex(&tx()),"tip_hash":tip()}),
                )
                .matches(|r| {
                    r.headers.as_ref().is_none_or(|h| {
                        h.iter().all(|(name, _)| {
                            ![
                                "authorization",
                                "cookie",
                                "x-device-fingerprint",
                                "x-device-name",
                                "referer",
                            ]
                            .iter()
                            .any(|forbidden| name.eq_ignore_ascii_case(forbidden))
                        })
                    })
                });
            then.status(200)
                .header("Cache-Control", "no-store")
                .json_body(response(chain, now()));
        });
        let result = client.observe(chain, &tx(), tip(), policy()).await.unwrap();
        assert_eq!(result.chain(), chain);
        mock.assert_hits(1);
    }
}
#[tokio::test]
async fn unsupported_invalid_and_revoked_requests_perform_no_io() {
    let server = MockServer::start();
    let (client, sender) = client(&server);
    let any = server.mock(|_, then| {
        then.status(500);
    });
    for chain in [
        ChainId::Testnet4,
        ChainId::BitcoinBlake2bTestnet4,
        ChainId::Testnet,
    ] {
        assert_eq!(
            client.observe(chain, &tx(), tip(), policy()).await,
            Err(Error::UnsupportedChain)
        );
    }
    let mut oversized = tx();
    oversized.input[0].witness.push(vec![0; 400001]);
    assert_eq!(
        client
            .observe(ChainId::Bitcoin, &oversized, tip(), policy())
            .await,
        Err(Error::InvalidRequest)
    );
    sender.send(8).unwrap();
    assert_eq!(
        client
            .observe(ChainId::Bitcoin, &tx(), tip(), policy())
            .await,
        Err(Error::Cancelled)
    );
    any.assert_hits(0);
}
#[tokio::test]
async fn response_markers_size_json_redirect_and_capacity_fail_closed() {
    for case in 0..5 {
        let server = MockServer::start();
        let (client, _sender) = client(&server);
        let mock = server.mock(|when, then| {
            when.method(POST)
                .path("/api/v1/esplora/bitcoin-blake2b/mainnet/tx/preflight");
            match case {
                0 => {
                    then.status(200)
                        .json_body(response(ChainId::BitcoinBlake2b, now()));
                }
                1 => {
                    then.status(200)
                        .header("Cache-Control", "no-store")
                        .body("x".repeat(MAX_RESPONSE_BYTES + 1));
                }
                2 => {
                    then.status(200)
                        .header("Cache-Control", "no-store")
                        .body("{malformed");
                }
                3 => {
                    then.status(302)
                        .header("Cache-Control", "no-store")
                        .header("Location", format!("{}/forbidden", server.base_url()));
                }
                _ => {
                    then.status(503)
                        .header("Cache-Control", "no-store")
                        .header("Retry-After", "10")
                        .json_body(json!({"success":false,"error":{"code":"SERVICE_UNAVAILABLE"}}));
                }
            }
        });
        let forbidden = server.mock(|when, then| {
            when.path("/forbidden");
            then.status(200);
        });
        let expected = match case {
            0 => Error::MissingNoStore,
            1 => Error::ResponseTooLarge,
            2 => Error::InvalidResponse,
            3 => Error::Http {
                status: 302,
                retry_after_seconds: None,
            },
            _ => Error::Http {
                status: 503,
                retry_after_seconds: Some(10),
            },
        };
        assert_eq!(
            client
                .observe(ChainId::BitcoinBlake2b, &tx(), tip(), policy())
                .await,
            Err(expected)
        );
        mock.assert_hits(1);
        forbidden.assert_hits(0);
    }
}
#[tokio::test]
async fn generation_cancellation_discards_inflight_response() {
    let server = MockServer::start();
    let (client, sender) = client(&server);
    server.mock(|when, then| {
        when.path("/api/v1/esplora/bitcoin/mainnet/tx/preflight");
        then.status(200)
            .delay(Duration::from_secs(2))
            .header("Cache-Control", "no-store")
            .json_body(response(ChainId::Bitcoin, now()));
    });
    let transaction = tx();
    let revoke = async {
        tokio::time::sleep(Duration::from_millis(30)).await;
        sender.send(8).unwrap();
    };
    let (result, ()) = tokio::join!(
        client.observe(ChainId::Bitcoin, &transaction, tip(), policy()),
        revoke
    );
    assert_eq!(result, Err(Error::Cancelled));
}
