//! The Electrum backend's fallible mempool reads against a scripted server
//! (#597): "the server does not know it" is absence, a dropped connection is an
//! error.

use super::*;
use crate::bitcoin::BitcoinInterface;
use bitcoin::{absolute, consensus::encode::serialize_hex, transaction::Version};
use serde_json::Value as Json;
use std::{
    io::{self, BufRead, BufReader, Write},
    net::TcpListener,
    str::FromStr,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
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

/// Run `call` against an Electrum backend (no retries) connected to a server
/// answering with `answer`.
fn against_server<T>(
    answer: impl Fn(&str, &Json) -> Answer + Send + Sync + 'static,
    call: impl FnOnce(&Electrum) -> T,
) -> T {
    const DESCRIPTOR: &str = concat!(
        "wsh(andor(pk([aabbccdd]xpub68JJTXc1MWK8KLW4HGLXZBJknja7kDUJuFHnM424LbziEXsfkh1WQCiEjjHw4z",
        "LqSUm4rvhgyGkkuRowE9tCJSgt3TQB5J3SKAbZ2SdcKST/<0;1>/*),older(10000),pk([aabbccdd]xpub68JJT",
        "Xc1MWK8PEQozKsRatrUHXKFNkD1Cb1BuQU9Xr5moCv87anqGyXLyUd4KpnDyZgo3gz4aN1r3NiaoweFW8Uut",
        "BsBbgKHzaD5HkTkifK/<0;1>/*)))#3xh8xmhn"
    );
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
    let result = call(&backend);
    drop(backend);
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
