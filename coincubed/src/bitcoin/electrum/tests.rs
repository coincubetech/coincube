//! The Electrum backend's fallible reads against a scripted server: "the server
//! does not know it" is absence, a dropped connection is an error (#597), and so
//! is a block height out of our range (#616).

use super::*;
use crate::bitcoin::BitcoinInterface;
use bitcoin::{absolute, consensus::encode::serialize_hex, transaction::Version};
use serde_json::Value as Json;
use std::{
    io::{self, BufRead, BufReader, Write},
    net::TcpListener,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

/// How the scripted server answers one request.
enum Answer {
    Result(Json),
    /// A JSON-RPC error object: the server answered, with an error.
    Error(Json),
    /// Close the connection without answering, and refuse every later one.
    Drop,
}

/// A regtest Electrum server whose chain is the genesis block alone, answering
/// every request with `answer(method, params)`.
fn scripted_server(
    listener: TcpListener,
    answer: impl Fn(&str, &Json) -> Answer + Send + Sync + 'static,
    stop: mpsc::Receiver<()>,
) -> thread::JoinHandle<()> {
    // Nonblocking on this handle, so the stop signal is seen between
    // connections (a blocking accept hung the Windows CI job in #591).
    listener.set_nonblocking(true).unwrap();
    let answer = Arc::new(answer);
    let down = Arc::new(AtomicBool::new(false));
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let stream = match listener.accept() {
                Ok((stream, _)) => stream,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    if stop.try_recv().is_ok() {
                        return;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "scripted server was never stopped"
                    );
                    thread::sleep(Duration::from_millis(5));
                    continue;
                }
                other => panic!("Electrum test accept failed: {:?}", other),
            };
            if down.load(Ordering::SeqCst) {
                continue;
            }
            // Accepted sockets can inherit the listener's nonblocking mode.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(30)))
                .unwrap();
            let (answer, down) = (answer.clone(), down.clone());
            // One thread per connection: the client keeps it open between
            // requests, and the ping-only probe in `Client::new` has its own.
            thread::spawn(move || {
                let mut writer = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { return };
                    let request: Json = serde_json::from_str(&line).unwrap();
                    let requests = match request {
                        Json::Array(requests) => requests,
                        request => vec![request],
                    };
                    let mut replies = Vec::new();
                    for request in requests {
                        let method = request["method"].as_str().unwrap();
                        let (key, value) = match answer(method, &request["params"]) {
                            Answer::Result(result) => ("result", result),
                            Answer::Error(error) => ("error", error),
                            Answer::Drop => {
                                down.store(true, Ordering::SeqCst);
                                return;
                            }
                        };
                        replies.push(
                            serde_json::json!({"jsonrpc": "2.0", "id": request["id"], key: value}),
                        );
                    }
                    for reply in replies {
                        if writeln!(writer, "{}", reply).is_err() {
                            return;
                        }
                    }
                }
            });
        }
    })
}

/// The chain reads every mempool read starts with, for a genesis-only chain.
fn chain_answer(method: &str) -> Option<Answer> {
    let header = serialize_hex(
        &bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest).header,
    );
    Some(Answer::Result(match method {
        "server.ping" => Json::Null,
        "blockchain.block.header" => Json::String(header),
        "blockchain.headers.subscribe" => serde_json::json!({"height": 0, "hex": header}),
        "blockchain.block.headers" => serde_json::json!({"count": 1, "hex": header, "max": 2016}),
        _ => return None,
    }))
}

pub(crate) const DESCRIPTOR: &str = concat!(
    "wsh(andor(pk([aabbccdd]xpub68JJTXc1MWK8KLW4HGLXZBJknja7kDUJuFHnM424LbziEXsfkh1WQCiEjjHw4z",
    "LqSUm4rvhgyGkkuRowE9tCJSgt3TQB5J3SKAbZ2SdcKST/<0;1>/*),older(10000),pk([aabbccdd]xpub68JJT",
    "Xc1MWK8PEQozKsRatrUHXKFNkD1Cb1BuQU9Xr5moCv87anqGyXLyUd4KpnDyZgo3gz4aN1r3NiaoweFW8Uut",
    "BsBbgKHzaD5HkTkifK/<0;1>/*)))#3xh8xmhn"
);

/// Run `call` against an Electrum backend (no retries) connected to a server
/// answering with `answer`. `call` owns the backend, so it can put it behind
/// the lock the daemon shares it through.
fn against_server<T>(
    answer: impl Fn(&str, &Json) -> Answer + Send + Sync + 'static,
    call: impl FnOnce(Electrum) -> T,
) -> T {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("tcp://{}", listener.local_addr().unwrap());
    let (stop, stopped) = mpsc::channel();
    let server = scripted_server(listener, answer, stopped);
    let client = client::Client::with_retries(
        &crate::config::ElectrumConfig {
            addr,
            validate_domain: false,
        },
        0,
    )
    .unwrap();
    let genesis_hash =
        bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest).block_hash();
    let wallet = wallet::BdkWallet::new(
        &DESCRIPTOR.parse().unwrap(),
        genesis_hash,
        None,
        &[],
        &[],
        0.into(),
        0.into(),
    );
    let backend = Electrum::new(client, wallet, false).unwrap();
    let result = call(backend);
    stop.send(()).unwrap();
    server.join().unwrap();
    result
}

// How electrum-client reports a request that failed on the connection, after
// its retries (`Error::AllAttemptsErrored`): not a server error reply.
const TRANSPORT_FAILURE: &str = "Made one or multiple attempts, all errored";

// Electrum's reply for a transaction it does not know.
fn unknown_tx() -> Answer {
    Answer::Error(serde_json::json!({
        "code": 2,
        "message": "daemon error: No such mempool or blockchain transaction."
    }))
}

#[test]
fn electrum_mempool_entry_is_absent_for_an_unknown_tx_and_an_error_in_an_outage() {
    let txid =
        bitcoin::Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
            .unwrap();

    // The server answers that it does not know the transaction: not in the mempool.
    let entry = against_server(
        |method, _| {
            chain_answer(method).unwrap_or_else(|| match method {
                "blockchain.transaction.get" => unknown_tx(),
                other => panic!("unexpected Electrum request {}", other),
            })
        },
        |backend| backend.mempool_entry_result(&txid),
    );
    assert!(matches!(entry, Ok(None)), "{:?}", entry);

    // The server goes away at each read in turn: an error, never "not in the mempool".
    for outage in [
        "blockchain.block.header",
        "blockchain.headers.subscribe",
        "blockchain.block.headers",
        "blockchain.transaction.get",
    ] {
        let entry = against_server(
            move |method, _| {
                if method == outage {
                    return Answer::Drop;
                }
                chain_answer(method).unwrap_or_else(unknown_tx)
            },
            |backend| backend.mempool_entry_result(&txid),
        );
        assert!(
            matches!(&entry, Err(e) if e.contains(TRANSPORT_FAILURE)),
            "outage at {} returned {:?}",
            outage,
            entry
        );
    }
}

#[test]
fn electrum_mempool_spenders_are_empty_for_an_unspent_outpoint_and_an_error_in_an_outage() {
    // An unconfirmed transaction whose output nothing spends.
    let funding = bitcoin::Transaction {
        version: Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![bitcoin::TxIn {
            previous_output: OutPoint::null(),
            script_sig: bitcoin::ScriptBuf::from_bytes(vec![0x51]),
            sequence: bitcoin::Sequence::MAX,
            witness: bitcoin::Witness::default(),
        }],
        output: vec![bitcoin::TxOut {
            value: bitcoin::Amount::from_sat(100_000),
            script_pubkey: bitcoin::ScriptBuf::from_bytes(vec![0x51]),
        }],
    };
    let outpoint = OutPoint::new(funding.compute_txid(), 0);
    let answer = move |method: &str| {
        chain_answer(method).unwrap_or_else(|| match method {
            "blockchain.transaction.get" => Answer::Result(Json::String(serialize_hex(&funding))),
            // The funding transaction is the script's only history: no spender.
            "blockchain.scripthash.get_history" => Answer::Result(serde_json::json!([
                {"tx_hash": outpoint.txid.to_string(), "height": 0}
            ])),
            other => panic!("unexpected Electrum request {}", other),
        })
    };
    let answer = Arc::new(answer);

    let spenders = {
        let answer = answer.clone();
        against_server(
            move |method, _| answer(method),
            |backend| backend.mempool_spenders_result(&[outpoint]),
        )
    };
    assert!(
        matches!(&spenders, Ok(spenders) if spenders.is_empty()),
        "{:?}",
        spenders
    );

    // The server goes away at each read in turn: an error, never "no spender",
    // which would drop the RBF minimum feerate.
    for outage in [
        "blockchain.block.header",
        "blockchain.headers.subscribe",
        "blockchain.block.headers",
        "blockchain.transaction.get",
        "blockchain.scripthash.get_history",
    ] {
        let answer = answer.clone();
        let spenders = against_server(
            move |method, _| {
                if method == outage {
                    return Answer::Drop;
                }
                answer(method)
            },
            |backend| backend.mempool_spenders_result(&[outpoint]),
        );
        assert!(
            matches!(&spenders, Err(e) if e.contains(TRANSPORT_FAILURE)),
            "outage at {} returned {:?}",
            outage,
            spenders
        );
    }
}

#[test]
fn electrum_mempool_entry_with_a_refused_ancestor_parent_is_an_error_not_a_panic() {
    // T spends the unconfirmed A, which spends Q. The server serves T and A
    // but answers "no such transaction" for Q, so A's fee cannot be computed:
    // this used to panic under the backend lock (MissingTxOut).
    let q =
        bitcoin::Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
            .unwrap();
    let spend = |prevout: OutPoint, value: u64| bitcoin::Transaction {
        version: Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![bitcoin::TxIn {
            previous_output: prevout,
            script_sig: bitcoin::ScriptBuf::new(),
            sequence: bitcoin::Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: bitcoin::Witness::default(),
        }],
        output: vec![bitcoin::TxOut {
            value: bitcoin::Amount::from_sat(value),
            script_pubkey: bitcoin::ScriptBuf::from_bytes(vec![0x51]),
        }],
    };
    let a = spend(OutPoint::new(q, 0), 90_000);
    let t = spend(OutPoint::new(a.compute_txid(), 0), 80_000);
    let (a_id, t_id) = (a.compute_txid(), t.compute_txid());
    let answer = Arc::new(move |method: &str, params: &Json| {
        chain_answer(method).unwrap_or_else(|| match method {
            "blockchain.transaction.get" => match params[0].as_str() {
                Some(id) if id == a_id.to_string() => {
                    Answer::Result(Json::String(serialize_hex(&a)))
                }
                Some(id) if id == t_id.to_string() => {
                    Answer::Result(Json::String(serialize_hex(&t)))
                }
                _ => unknown_tx(),
            },
            // Both unconfirmed, paying to the same script.
            "blockchain.scripthash.get_history" => Answer::Result(serde_json::json!([
                {"tx_hash": a_id.to_string(), "height": 0},
                {"tx_hash": t_id.to_string(), "height": 0},
            ])),
            other => panic!("unexpected Electrum request {}", other),
        })
    });

    let entry = {
        let answer = answer.clone();
        against_server(
            move |method, params| answer(method, params),
            |backend| backend.mempool_entry_result(&t_id),
        )
    };
    assert!(
        matches!(&entry, Err(e) if e.contains("cannot compute the mempool fees")),
        "{:?}",
        entry
    );
    // The same walk runs for the spenders of A's output (T), as rbfpsbt reads them.
    let spenders = against_server(
        move |method, params| answer(method, params),
        |backend| backend.mempool_spenders_result(&[OutPoint::new(a_id, 0)]),
    );
    assert!(
        matches!(&spenders, Err(e) if e.contains("cannot compute the mempool fees")),
        "{:?}",
        spenders
    );
}

/// A server whose chain is the genesis block, except that it reports its tip at
/// `height` (read at each request, so a test can move it). Every block header
/// it serves is the genesis header. It knows no transaction and no script history.
fn tip_at(height: Arc<AtomicU64>) -> impl Fn(&str, &Json) -> Answer + Send + Sync + 'static {
    move |method, _| {
        let header = serialize_hex(
            &bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest).header,
        );
        match method {
            "blockchain.headers.subscribe" => Answer::Result(
                serde_json::json!({"height": height.load(Ordering::SeqCst), "hex": header}),
            ),
            "blockchain.scripthash.get_history" => Answer::Result(serde_json::json!([])),
            "blockchain.transaction.get" => unknown_tx(),
            other => chain_answer(other)
                .unwrap_or_else(|| panic!("unexpected Electrum request {}", other)),
        }
    }
}

const OUT_OF_RANGE: &str = "out-of-range block height";

#[test]
fn electrum_out_of_range_tip_height_is_an_error_for_commands_under_the_lock() {
    let txid =
        bitcoin::Txid::from_str("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b")
            .unwrap();
    let outpoint = OutPoint::new(txid, 0);
    for height in [i32::MAX as u64 + 1, u32::MAX as u64, 1 << 40] {
        against_server(tip_at(Arc::new(AtomicU64::new(height))), |backend| {
            // As the daemon shares it: every command read takes this lock, and
            // a panic while holding it poisons it for the poller and the GUI.
            let shared: Arc<Mutex<dyn BitcoinInterface>> = Arc::new(Mutex::new(backend));

            let entry = shared.mempool_entry_result(&txid);
            assert!(
                matches!(&entry, Err(e) if e.contains(OUT_OF_RANGE)),
                "height {}: {:?}",
                height,
                entry
            );
            let spenders = shared.mempool_spenders_result(&[outpoint]);
            assert!(
                matches!(&spenders, Err(e) if e.contains(OUT_OF_RANGE)),
                "height {}: {:?}",
                height,
                spenders
            );
            // The infallible reads degrade to "unknown" rather than panicking.
            assert_eq!(shared.tip_time(), None, "height {}", height);
            assert!(shared.mempool_entry(&txid).is_none(), "height {}", height);
            assert!(
                shared.mempool_spenders(&[outpoint]).is_empty(),
                "height {}",
                height
            );

            assert!(!shared.is_poisoned(), "height {}", height);
        });
    }
}

/// Poll `shared` against a server reporting an out-of-range tip: the poll fails,
/// and nothing of the update was applied, so the poller's next reads of the tip
/// see `expected_tip` instead of panicking under the lock.
fn assert_poll_refused(shared: &mut Arc<Mutex<dyn BitcoinInterface>>, expected_tip: i32) {
    let genesis_hash =
        bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest).block_hash();
    let sync = shared.sync_wallet(0.into(), 0.into());
    assert!(
        matches!(&sync, Err(e) if e.contains(OUT_OF_RANGE)),
        "{:?}",
        sync
    );
    let tip = shared.chain_tip();
    // Every header the scripted server serves is the genesis header.
    assert_eq!((tip.height, tip.hash), (expected_tip, genesis_hash));
    assert_eq!(shared.sync_progress().blocks, expected_tip as u64);
    assert!(!shared.is_poisoned());
}

#[test]
fn electrum_out_of_range_tip_height_fails_the_poll_and_leaves_the_wallet_tip_alone() {
    // BDK reads the server's tip as a `u32`, so these are the heights a sync can
    // be handed that do not fit into ours. It asks for the last eight blocks up
    // to the tip and the scripted server returns one, so the update's tip is the
    // first block of that range: seven below the reported height.
    for out_of_range in [i32::MAX as u64 + 10, u32::MAX as u64] {
        let height = Arc::new(AtomicU64::new(out_of_range));
        against_server(tip_at(height.clone()), |backend| {
            let mut shared: Arc<Mutex<dyn BitcoinInterface>> = Arc::new(Mutex::new(backend));

            // The wallet's chain is at genesis: the poll is a full scan, and the
            // next one is still a full scan.
            assert_poll_refused(&mut shared, 0);
            assert_eq!(shared.rescan_progress(), Some(0.0));
            assert_poll_refused(&mut shared, 0);

            // A sane tip, 20: the update's tip is block 13 and the wallet's chain
            // moves there, so the next poll is an incremental sync.
            height.store(20, Ordering::SeqCst);
            let sync = shared.sync_wallet(0.into(), 0.into());
            assert!(matches!(sync, Ok(None)), "{:?}", sync);
            assert_eq!(shared.chain_tip().height, 13);
            assert_eq!(shared.rescan_progress(), None);

            height.store(out_of_range, Ordering::SeqCst);
            assert_poll_refused(&mut shared, 13);
            assert_eq!(shared.rescan_progress(), None);
        });
    }
}

#[test]
fn electrum_refused_poll_during_a_rescan_keeps_the_rescan_and_warns() {
    let height = Arc::new(AtomicU64::new(20));
    against_server(tip_at(height.clone()), |backend| {
        let mut shared: Arc<Mutex<dyn BitcoinInterface>> = Arc::new(Mutex::new(backend));
        // A sane first poll: the wallet's chain moves to block 13 (see above), so
        // only an explicit rescan makes the next poll a full scan.
        let sync = shared.sync_wallet(0.into(), 0.into());
        assert!(matches!(sync, Ok(None)), "{:?}", sync);
        assert_eq!(shared.chain_tip().height, 13);
        assert_eq!(shared.rescan_progress(), None);

        // The user asks for a rescan, and its first poll is refused.
        let desc = DESCRIPTOR.parse().unwrap();
        shared.start_rescan(&desc, 0).unwrap();
        assert_eq!(shared.rescan_progress(), Some(0.0));
        height.store(u32::MAX as u64, Ordering::SeqCst);
        let ((), logs) = crate::testutils::capture_logs(|| assert_poll_refused(&mut shared, 13));
        // Still a rescan: had the refusal cleared it, the poller would take the
        // rescan for complete and the wallet would never be rescanned.
        assert_eq!(shared.rescan_progress(), Some(0.0));
        // Visible at the default log level, with the reason and no server address.
        let warnings: Vec<_> = logs
            .iter()
            .filter(|(level, _)| *level == log::Level::Warn)
            .map(|(_, message)| message)
            .collect();
        assert_eq!(warnings.len(), 1, "{:?}", logs);
        assert!(
            warnings[0].contains("Refused the Electrum chain update")
                && warnings[0].contains("out of range")
                && !warnings[0].contains("127.0.0.1"),
            "{:?}",
            warnings
        );

        // The next sane poll is the full scan, and it completes the rescan.
        height.store(20, Ordering::SeqCst);
        let sync = shared.sync_wallet(0.into(), 0.into());
        assert!(sync.is_ok(), "{:?}", sync);
        assert_eq!(shared.rescan_progress(), None);
        assert!(!shared.is_poisoned());
    });
}

/// A server like [`tip_at`] whose tip moves up ten blocks at every tip request,
/// for the first `moving` requests, then stays put. `subscribes` counts them.
fn moving_tip(
    subscribes: Arc<AtomicU64>,
    moving: u64,
    rest: impl Fn(&str, &Json) -> Answer + Send + Sync + 'static,
) -> impl Fn(&str, &Json) -> Answer + Send + Sync + 'static {
    move |method, params| {
        if method == "blockchain.headers.subscribe" {
            let calls = subscribes.fetch_add(1, Ordering::SeqCst) + 1;
            let header = serialize_hex(
                &bitcoin::blockdata::constants::genesis_block(bitcoin::Network::Regtest).header,
            );
            let height = 100 + 10 * calls.min(moving);
            return Answer::Result(serde_json::json!({"height": height, "hex": header}));
        }
        rest(method, params)
    }
}

#[test]
fn electrum_mempool_walks_give_up_when_the_tip_keeps_changing() {
    // T spends Q's output, unconfirmed, paying to a script whose only history is T.
    let q = bitcoin::Transaction {
        version: Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![bitcoin::TxIn {
            previous_output: OutPoint::null(),
            script_sig: bitcoin::ScriptBuf::from_bytes(vec![0x51]),
            sequence: bitcoin::Sequence::MAX,
            witness: bitcoin::Witness::default(),
        }],
        output: vec![bitcoin::TxOut {
            value: bitcoin::Amount::from_sat(100_000),
            script_pubkey: bitcoin::ScriptBuf::from_bytes(vec![0x52]),
        }],
    };
    let q_id = q.compute_txid();
    let t = bitcoin::Transaction {
        version: Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![bitcoin::TxIn {
            previous_output: OutPoint::new(q_id, 0),
            script_sig: bitcoin::ScriptBuf::new(),
            sequence: bitcoin::Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: bitcoin::Witness::default(),
        }],
        output: vec![bitcoin::TxOut {
            value: bitcoin::Amount::from_sat(80_000),
            script_pubkey: bitcoin::ScriptBuf::from_bytes(vec![0x51]),
        }],
    };
    let t_id = t.compute_txid();
    let rest = Arc::new(move |method: &str, params: &Json| {
        chain_answer(method).unwrap_or_else(|| match method {
            "blockchain.transaction.get" => match params[0].as_str() {
                Some(id) if id == t_id.to_string() => {
                    Answer::Result(Json::String(serialize_hex(&t)))
                }
                Some(id) if id == q_id.to_string() => {
                    Answer::Result(Json::String(serialize_hex(&q)))
                }
                _ => unknown_tx(),
            },
            "blockchain.scripthash.get_history" => Answer::Result(serde_json::json!([
                {"tx_hash": t_id.to_string(), "height": 0},
            ])),
            other => panic!("unexpected Electrum request {}", other),
        })
    });
    // Moving for far longer than the walks may retry, but not for ever: before
    // the bound, they recursed until the tip stopped moving.
    const MOVING: u64 = 200;
    const KEPT_CHANGING: &str = "the chain tip kept changing";

    // Each attempt reads the tip a handful of times: once itself and once per BDK
    // sync, five with this server. Four attempts (one and three restarts) read it
    // at most 4 * 5 times; an unbounded walk reads it until it stops moving.
    const MAX_READS: u64 = 4 * 5;

    // The entry walk restarts when the tip moves under one of its syncs.
    let subscribes = Arc::new(AtomicU64::new(0));
    let entry = {
        let rest = rest.clone();
        against_server(
            moving_tip(subscribes.clone(), MOVING, move |m, p| rest(m, p)),
            |backend| {
                let shared: Arc<Mutex<dyn BitcoinInterface>> = Arc::new(Mutex::new(backend));
                let entry = shared.mempool_entry_result(&t_id);
                assert!(!shared.is_poisoned());
                entry
            },
        )
    };
    assert!(
        matches!(&entry, Err(e) if e.contains(KEPT_CHANGING)),
        "{:?}",
        entry
    );
    let calls = subscribes.load(Ordering::SeqCst);
    assert!(calls <= MAX_READS, "{} tip reads", calls);

    // The spenders walk restarts when the tip moves between its own sync and the
    // entry walk's. It runs twice here: fallible, then infallible.
    let subscribes = Arc::new(AtomicU64::new(0));
    let spenders = against_server(
        moving_tip(subscribes.clone(), MOVING, move |m, p| rest(m, p)),
        |backend| {
            let shared: Arc<Mutex<dyn BitcoinInterface>> = Arc::new(Mutex::new(backend));
            let spenders = shared.mempool_spenders_result(&[OutPoint::new(t_id, 0)]);
            // The infallible read degrades to "no spender" rather than spinning.
            assert!(shared
                .mempool_spenders(&[OutPoint::new(t_id, 0)])
                .is_empty());
            assert!(!shared.is_poisoned());
            spenders
        },
    );
    assert!(
        matches!(&spenders, Err(e) if e.contains(KEPT_CHANGING)),
        "{:?}",
        spenders
    );
    let calls = subscribes.load(Ordering::SeqCst);
    assert!(calls <= 2 * MAX_READS, "{} tip reads", calls);
}
