use super::*;
use bitcoin::hashes::Hash;
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
    time::Instant,
};

fn backend(addr: SocketAddr) -> BitcoinD {
    let config = config::BitcoindConfig {
        addr,
        rpc_auth: config::BitcoindRpcAuth::UserPass("test".into(), "test".into()),
    };
    let client = |kind| RwLock::new(BitcoinD::build_client(&config, "poll-test", kind).unwrap());
    BitcoinD {
        poll_node_client: client(ClientKind::PollNode),
        poll_wallet_client: client(ClientKind::PollWallet),
        poll_abort: Default::default(),
        node_client: client(ClientKind::Node),
        watchonly_client: client(ClientKind::Watchonly),
        sendonly_client: client(ClientKind::Sendonly),
        sendonly_node_client: client(ClientKind::SendonlyNode),
        watchonly_wallet_path: "poll-test".into(),
        retries: BITCOIND_RETRY_LIMIT,
        config,
    }
}

// One bounded HTTP exchange. Matching the request ID exercises the real RPC
// decoder; tests do not replace BitcoinD's transport or classify fake errors.
fn response<T>(
    method: &'static str,
    result: Json,
    error: Json,
    call: impl FnOnce(&BitcoinD) -> T,
) -> T {
    responses(vec![(method, result, error)], call)
}

fn responses<T>(
    exchanges: Vec<(&'static str, Json, Json)>,
    call: impl FnOnce(&BitcoinD) -> T,
) -> T {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let bit = backend(listener.local_addr().unwrap());
    let worker = thread::spawn(move || {
        for (method, result, error) in exchanges {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5))
                    }
                    other => panic!("RPC test accept failed: {:?}", other),
                }
            };
            // Accepted sockets can inherit the listener's nonblocking mode.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut data = Vec::new();
            let mut byte = [0; 1];
            while !data.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                data.push(byte[0]);
                assert!(data.len() < 65536);
            }
            let head = String::from_utf8(data).unwrap();
            let len: usize = head
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            assert!(len < 65536);
            let mut body = vec![0; len];
            stream.read_exact(&mut body).unwrap();
            let request: Json = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["method"], method);
            let body =
                serde_json::json!({"result":result,"error":error,"id":request["id"]}).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        }
    });
    let result = call(&bit);
    worker.join().unwrap();
    result
}

#[test]
fn unreachable_node_is_not_absence_or_rescan_completion() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let bit = backend(listener.local_addr().unwrap());
    drop(listener);
    let txid = bitcoin::Txid::all_zeros();
    let hash = bitcoin::BlockHash::all_zeros();
    let start = Instant::now();
    assert!(bit.try_is_in_mempool(&txid).is_err());
    assert!(bit.try_get_transaction(&txid).is_err());
    assert!(bit
        .try_is_spent(&bitcoin::OutPoint { txid, vout: 0 })
        .is_err());
    assert!(bit.try_rescan_progress().is_err());
    assert!(bit.try_list_since_block(&hash).is_err());
    assert!(bit.try_chain_tip().is_err());
    assert!(bit.try_sync_progress().is_err());
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "polling must not enter the minute-long retry loop"
    );
}

#[test]
fn rpc_absence_is_distinct_from_operational_failure() {
    let txid = bitcoin::Txid::all_zeros();
    for code in [-5, -28, -1] {
        let err = serde_json::json!({"code":code,"message":"synthetic RPC error"});
        let mempool = response("getmempoolentry", Json::Null, err.clone(), |bit| {
            bit.try_is_in_mempool(&txid)
        });
        let transaction = response("gettransaction", Json::Null, err, |bit| {
            bit.try_get_transaction(&txid)
        });
        if code == -5 {
            assert!(!mempool.unwrap());
            assert!(transaction.unwrap().is_none());
        } else {
            assert!(mempool.is_err());
            assert!(transaction.is_err());
        }
    }
}

#[test]
fn only_explicit_idle_rescan_status_means_completion() {
    let idle = response(
        "getwalletinfo",
        serde_json::json!({"scanning":false}),
        Json::Null,
        BitcoinD::try_rescan_progress,
    );
    assert_eq!(idle.unwrap(), None);
    let active = response(
        "getwalletinfo",
        serde_json::json!({"scanning":{"progress":0.5}}),
        Json::Null,
        BitcoinD::try_rescan_progress,
    );
    assert_eq!(active.unwrap(), Some(0.5));
    for value in [
        serde_json::json!({}),
        serde_json::json!({"scanning":true}),
        serde_json::json!({"scanning":{"progress":2}}),
    ] {
        assert!(response(
            "getwalletinfo",
            value,
            Json::Null,
            BitcoinD::try_rescan_progress
        )
        .is_err());
    }
}

#[test]
fn a_hung_poll_is_bounded_and_shutdown_prevents_followup_reads() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let bit = backend(listener.local_addr().unwrap());
    let abort = bit.poll_abort.clone();
    let (accepted, acceptance) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(5))
                }
                other => panic!("hung RPC test accept failed: {:?}", other),
            }
        };
        accepted.send(()).unwrap();
        let _ = released.recv_timeout(Duration::from_secs(POLL_RPC_SOCKET_TIMEOUT + 5));
        drop(stream);
    });
    let started = Instant::now();
    let request = thread::spawn(move || {
        let result = bit.try_chain_tip();
        (bit, result)
    });
    acceptance.recv_timeout(Duration::from_secs(5)).unwrap();
    abort.store(true, std::sync::atomic::Ordering::Relaxed);
    let (bit, result) = request.join().unwrap();
    assert!(result.unwrap_err().contains("cancelled"));
    assert!(started.elapsed() < Duration::from_secs(POLL_RPC_SOCKET_TIMEOUT + 3));
    assert!(matches!(
        bit.poll_node("getblockchaininfo", None),
        Err(BitcoindError::PollAborted)
    ));
    release.send(()).unwrap();
    worker.join().unwrap();
}

#[test]
fn malformed_successful_wallet_responses_are_errors_not_panics() {
    let txid = bitcoin::Txid::all_zeros();
    let hash = bitcoin::BlockHash::all_zeros();
    for value in [
        serde_json::json!({}),
        serde_json::json!({"hex":"zz", "confirmations":0}),
        serde_json::json!({"hex":"00", "confirmations":0}),
    ] {
        assert!(response("gettransaction", value, Json::Null, |bit| bit
            .try_get_transaction(&txid))
        .is_err());
    }
    for value in [
        serde_json::json!({}),
        serde_json::json!({"transactions":[{}]}),
        serde_json::json!({"transactions":[{"category":"receive","txid":"bad"}]}),
    ] {
        assert!(response("listsinceblock", value, Json::Null, |bit| bit
            .try_list_since_block(&hash))
        .is_err());
    }
    assert!(response(
        "listsinceblock",
        serde_json::json!({"transactions":[]}),
        Json::Null,
        |bit| bit.try_list_since_block(&hash)
    )
    .unwrap()
    .received_coins
    .is_empty());
}

#[test]
fn assumed_confirmations_are_neither_confirmed_nor_expired() {
    use crate::bitcoin::BitcoinInterface;
    let tx = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Bitcoin)
        .txdata
        .remove(0);
    let txid = tx.compute_txid();
    let op = bitcoin::OutPoint { txid, vout: 0 };
    let value = serde_json::json!({
        "hex": bitcoin::consensus::encode::serialize_hex(&tx),
        "confirmations": 0,
        "confirmations_assumed": 150,
        "generated": true,
        "blockhash": bitcoin::BlockHash::all_zeros(),
        "blockheight": 100,
        "blocktime": 123
    });
    let parsed = response("gettransaction", value.clone(), Json::Null, |bit| {
        bit.try_get_transaction(&txid)
    })
    .unwrap()
    .unwrap();
    assert!(parsed.has_assumed_confirmation());
    assert_eq!(parsed.confirmations, 0);
    let (confirmed, expired) = response("gettransaction", value.clone(), Json::Null, |bit| {
        bit.try_confirmed_coins(&[op])
    })
    .unwrap();
    assert!(confirmed.is_empty() && expired.is_empty());
    let (spent, expired) = response("gettransaction", value.clone(), Json::Null, |bit| {
        bit.try_spent_coins(&[(op, txid)])
    })
    .unwrap();
    assert!(spent.is_empty() && expired.is_empty());
    let (_, block) = response("gettransaction", value.clone(), Json::Null, |bit| {
        bit.try_wallet_transaction(&txid)
    })
    .unwrap()
    .unwrap();
    assert!(block.is_none());
    for assumed in [Json::Null, serde_json::json!(0), serde_json::json!(-1)] {
        let mut invalid = value.clone();
        invalid["confirmations_assumed"] = assumed;
        assert!(parse::transaction(invalid, txid).is_err());
    }
    let mut validated = value;
    validated["confirmations"] = serde_json::json!(150);
    validated
        .as_object_mut()
        .unwrap()
        .remove("confirmations_assumed");
    let (confirmed, expired) = response("gettransaction", validated, Json::Null, |bit| {
        bit.try_confirmed_coins(&[op])
    })
    .unwrap();
    assert_eq!(confirmed, vec![(op, 100, 123)]);
    assert!(expired.is_empty());
}

#[test]
fn an_assumed_conflicting_spend_does_not_expire_the_coin() {
    use crate::bitcoin::BitcoinInterface;
    let mut tx = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Bitcoin)
        .txdata
        .remove(0);
    let op = bitcoin::OutPoint {
        txid: bitcoin::Txid::all_zeros(),
        vout: 1,
    };
    tx.input[0].previous_output = op;
    let txid = tx.compute_txid();
    let mut conflict = tx.clone();
    conflict.lock_time = bitcoin::absolute::LockTime::from_consensus(1);
    let conflict_id = conflict.compute_txid();
    let pending = serde_json::json!({
        "hex": bitcoin::consensus::encode::serialize_hex(&tx),
        "confirmations": 0, "walletconflicts": [conflict_id]
    });
    let assumed = serde_json::json!({
        "hex": bitcoin::consensus::encode::serialize_hex(&conflict),
        "confirmations": 0, "confirmations_assumed": 150,
        "blockhash": bitcoin::BlockHash::all_zeros(),
        "blockheight": 100, "blocktime": 123
    });
    let (spent, expired) = responses(
        vec![
            ("gettransaction", pending.clone(), Json::Null),
            ("gettransaction", assumed.clone(), Json::Null),
        ],
        |bit| bit.try_spent_coins(&[(op, txid)]),
    )
    .unwrap();
    assert!(spent.is_empty() && expired.is_empty());
    let mut validated = assumed;
    validated["confirmations"] = serde_json::json!(150);
    validated
        .as_object_mut()
        .unwrap()
        .remove("confirmations_assumed");
    let (spent, expired) = responses(
        vec![
            ("gettransaction", pending, Json::Null),
            ("gettransaction", validated, Json::Null),
        ],
        |bit| bit.try_spent_coins(&[(op, txid)]),
    )
    .unwrap();
    assert_eq!(spent, vec![(op, conflict_id, 100, 123)]);
    assert!(expired.is_empty());
}
