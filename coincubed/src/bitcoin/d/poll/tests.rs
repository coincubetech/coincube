use super::*;
use crate::database::DatabaseInterface;
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
    assert!(bit.try_is_in_mempool(&txid).is_err());
    assert!(bit.try_get_transaction(&txid).is_err());
    assert!(bit
        .try_is_spent(&bitcoin::OutPoint { txid, vout: 0 })
        .is_err());
    assert!(bit.try_rescan_progress().is_err());
    assert!(bit.try_list_since_block(&hash).is_err());
    assert!(bit.try_chain_tip().is_err());
    assert!(bit.try_sync_progress().is_err());
}

#[test]
fn polling_transport_errors_make_one_attempt_per_read() {
    // Closed-port connection timing differs across operating systems. Count
    // real accepted connections instead of timing seven connection refusals.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut bit = backend(listener.local_addr().unwrap());
    // A regression into the generic retry path must fail without making the
    // test spend a minute on each of the seven reads.
    bit.retries = 2;
    let (stop, stopped) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut requests = 0;
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    requests += 1;
                    // Closing before an HTTP reply exercises a real retryable
                    // transport failure, not an RPC-level absence result.
                    drop(stream);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if stopped.try_recv().is_ok() {
                        return requests;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "polling transport did not finish"
                    );
                    thread::sleep(Duration::from_millis(5));
                }
                other => panic!("RPC test accept failed: {:?}", other),
            }
        }
    });
    let txid = bitcoin::Txid::all_zeros();
    let hash = bitcoin::BlockHash::all_zeros();
    let failures = [
        bit.try_is_in_mempool(&txid).is_err(),
        bit.try_get_transaction(&txid).is_err(),
        bit.try_is_spent(&bitcoin::OutPoint { txid, vout: 0 })
            .is_err(),
        bit.try_rescan_progress().is_err(),
        bit.try_list_since_block(&hash).is_err(),
        bit.try_chain_tip().is_err(),
        bit.try_sync_progress().is_err(),
    ];
    stop.send(()).unwrap();
    let requests = worker.join().unwrap();
    assert!(failures.iter().all(|failed| *failed));
    assert_eq!(requests, 7, "polling must not retry a transport failure");
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

// A node that answers `script` in order, then goes away: every later connection
// is closed before an HTTP reply (a real retryable transport failure) until
// `stop` fires. Unlike `responses`, a reply the client no longer reads (the
// fire-and-forget `importdescriptors`) is not an error.
fn scripted_node(
    listener: TcpListener,
    script: Vec<(&'static str, Json)>,
    stop: std::sync::mpsc::Receiver<()>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut script = script.into_iter();
        loop {
            let mut stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if stop.try_recv().is_ok() {
                        return;
                    }
                    assert!(Instant::now() < deadline, "scripted node was never stopped");
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                other => panic!("RPC test accept failed: {:?}", other),
            };
            let Some((method, result)) = script.next() else {
                drop(stream);
                continue;
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
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
            let mut body = vec![0; len];
            stream.read_exact(&mut body).unwrap();
            let request: Json = serde_json::from_slice(&body).unwrap();
            assert_eq!(request["method"], method, "unexpected RPC order");
            let body =
                serde_json::json!({"result":result,"error":null,"id":request["id"]}).to_string();
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
        }
    })
}

/// A daemon whose commands reach `addr` through the real bitcoind backend. The
/// poller was started against a dummy backend and is already shut down, so the
/// scripted node sees the command's reads and nothing else.
fn command_control(addr: SocketAddr) -> crate::DaemonControl {
    use crate::testutils::{DummyBitcoind, DummyCoincube, DummyDatabase};
    let daemon = DummyCoincube::new(DummyBitcoind::new(), DummyDatabase::new());
    let mut control = daemon.control().clone();
    daemon.shutdown();
    let mut bit = backend(addr);
    // One attempt per read: an outage "outlasting the retry budget" without
    // spending a minute per read.
    bit.retries = 0;
    control.bitcoin = std::sync::Arc::new(std::sync::Mutex::new(bit));
    control
}

fn rescan_against(
    control: &mut crate::DaemonControl,
    listener: &TcpListener,
    script: Vec<(&'static str, Json)>,
    timestamp: u32,
) -> Result<(), crate::commands::CommandError> {
    let (stop, stopped) = std::sync::mpsc::channel();
    let node = scripted_node(listener.try_clone().unwrap(), script, stopped);
    let result = control.start_rescan(timestamp);
    stop.send(()).unwrap();
    node.join().unwrap();
    result
}

#[test]
fn start_rescan_node_outage_is_a_retryable_error_not_a_panic() {
    use crate::commands::CommandError;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut control = command_control(listener.local_addr().unwrap());

    let genesis = bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Bitcoin);
    let genesis_hash = genesis.block_hash();
    let tip_hash = bitcoin::BlockHash::from_byte_array([1; 32]);
    let (genesis_time, tip_time, rescan_time) = (1_231_006_505u32, 1_700_000_000u32, 1_600_000_000);
    let header = |height: i32, time: u32| serde_json::json!({"confirmations":1,"height":height,"time":time,"mediantime":time});
    // An unpruned node: no `pruneheight`.
    let chain_info = serde_json::json!({"bestblockhash":tip_hash,"blocks":200});
    let descriptors = |timestamp: u32| {
        let desc = &control.config.main_descriptor;
        let entries: Vec<Json> = [desc.receive_descriptor(), desc.change_descriptor()]
            .iter()
            .map(|d| {
                serde_json::json!({
                    "desc": d.as_descriptor_public_key().to_string(),
                    "timestamp": timestamp,
                    "range": [0, 999],
                })
            })
            .collect();
        serde_json::json!({ "descriptors": entries })
    };
    // Every read `start_rescan` makes against bitcoind, in order.
    let script = vec![
        ("getblockhash", serde_json::json!(genesis_hash)),
        ("getblockheader", header(0, genesis_time)),
        ("getblockchaininfo", chain_info.clone()), // tip_time
        ("getblockheader", header(200, tip_time)),
        ("getwalletinfo", serde_json::json!({"scanning": false})),
        ("listdescriptors", descriptors(genesis_time)), // import range
        ("getblockchaininfo", chain_info),              // prune check
        ("importdescriptors", serde_json::json!([{"success": true}])),
        ("listdescriptors", descriptors(rescan_time)), // import confirmed
    ];

    // The node drops before each read in turn. Every such outage must come
    // back to the caller as an error, with no rescan recorded, where the reads
    // after the genesis lookup used to panic the daemon.
    for answered in 0..script.len() {
        let result = rescan_against(
            &mut control,
            &listener,
            script[..answered].to_vec(),
            rescan_time,
        );
        match (answered, result) {
            (0..=1, Err(CommandError::RescanGenesis(_))) => {}
            (2.., Err(CommandError::RescanTrigger(_))) => {}
            (answered, result) => {
                panic!(
                    "outage after {} answered reads returned {:?}",
                    answered, result
                )
            }
        }
        assert!(control.db.connection().rescan_timestamp().is_none());
    }

    // The node is back: the same command now succeeds and records the rescan.
    rescan_against(&mut control, &listener, script, rescan_time).unwrap();
    assert_eq!(
        control.db.connection().rescan_timestamp(),
        Some(rescan_time)
    );
}

#[test]
fn command_reads_during_a_node_outage_do_not_panic_or_claim_completion() {
    use crate::commands::CommandError;
    use std::str::FromStr;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let control = command_control(listener.local_addr().unwrap());
    drop(listener);

    // A pending rescan the node cannot report on is still pending.
    control.db.connection().set_rescan(1_600_000_000);
    let progress = control.get_info().rescan_progress.unwrap();
    assert!(
        progress < 1.0,
        "an outage reported rescan progress {}",
        progress
    );

    let address = bitcoin::Address::from_str("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4").unwrap();
    assert!(matches!(
        control.create_recovery(address, &[], 2, None),
        Err(CommandError::ChainTipUnavailable(_))
    ));
}
