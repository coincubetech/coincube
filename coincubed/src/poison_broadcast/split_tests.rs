//! Split (#568 B0b) daemonless step-1 transport tests. The step 1 is a real
//! core construction, signed by rust-bitcoin's reference PSBT signer and
//! verified by `finalize_split_step1`; the Connect endpoint is a local socket.
use super::*;
use coincube_core::{
    claim::BlockRef,
    foreign_split::{
        create_split_step1, finalize_split_step1, SplitBranch, SplitCoin, SplitInputs, SplitSource,
    },
};
use miniscript::{
    bitcoin::{
        self,
        absolute::LockTime,
        bip32::{DerivationPath, Xpriv, Xpub},
        hashes::Hash,
        secp256k1::Secp256k1,
        transaction, Amount, BlockHash, Network, OutPoint, TxIn, TxOut,
    },
    Descriptor,
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    str::FromStr,
    sync::Barrier,
    time::{Duration, Instant},
};

const FORK: u64 = 900;
const TIP: u32 = 960;

fn master(seed: u8) -> Xpriv {
    Xpriv::new_master(Network::Bitcoin, &[seed; 32]).unwrap()
}

/// A finalized single-key native segwit step 1 on `chain`. `seed` varies the
/// wallet and so the transaction.
fn split_step1(chain: ChainId, seed: u8) -> VerifiedSplitStep1 {
    let secp = Secp256k1::new();
    let signer = master(seed);
    let child = signer
        .derive_priv(&secp, &DerivationPath::from_str("m/84'/0'/0'").unwrap())
        .unwrap();
    let key = format!(
        "[{}/84'/0'/0']{}",
        signer.fingerprint(&secp),
        Xpub::from_priv(&secp, &child)
    );
    let branch = |b: u32| Descriptor::from_str(&format!("wpkh({}/{}/*)", key, b)).unwrap();
    let source = SplitSource::new(branch(0), Some(branch(1))).unwrap();
    let previous = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([seed; 32]), 0),
            ..TxIn::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(150_000),
            script_pubkey: source
                .external()
                .at_derivation_index(0)
                .unwrap()
                .script_pubkey(),
        }],
    };
    let block = BlockRef {
        height: FORK - 10,
        hash: BlockHash::from_byte_array([0x33; 32]),
    };
    let coins = [SplitCoin {
        outpoint: OutPoint::new(previous.compute_txid(), 0),
        branch: SplitBranch::External,
        index: 0,
        previous,
        bitcoin_block: Some(block),
        btcb2_block: Some(block),
    }];
    let step1 = create_split_step1(
        &SplitInputs {
            chain,
            source: &source,
            coins: &coins,
            fork_height: FORK,
            destination: 5,
        },
        2,
        LockTime::from_height(TIP).unwrap(),
        TIP,
        BlockHash::from_byte_array([7; 32]),
    )
    .unwrap();
    let mut psbt = step1.psbt().clone();
    psbt.sign(&signer, &secp).unwrap();
    finalize_split_step1(&step1, &psbt, &secp).unwrap()
}

fn fresh_gate(verified: &VerifiedSplitStep1) -> (SubmissionGate, SubmissionRevoker) {
    SubmissionGate::for_split_step1(verified, Instant::now() + Duration::from_secs(30))
}

/// No connection was ever attempted on `listener`.
fn untouched(listener: &TcpListener) -> bool {
    matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock)
}

/// (e) One attempt with exactly the verified witness bytes, no daemon and no
/// credentials. A gate is consumed by that attempt: a second call is
/// `AlreadyStarted` and never reaches the socket. Any response other than the
/// exact txid, and a redirect (never followed), is `Uncertain`.
#[test]
fn split_step1_connect_transport_sends_exact_bytes_once_without_a_daemon() {
    let verified = split_step1(ChainId::Bitcoin, 1);
    for case in 0..5 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let tx = verified.transaction().clone();
        let worker = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    other => panic!("connect fixture accept failed: {:?}", other),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut headers = Vec::new();
            let mut byte = [0];
            while !headers.ends_with(b"\r\n\r\n") {
                stream.read_exact(&mut byte).unwrap();
                headers.push(byte[0]);
                assert!(headers.len() < 8192);
            }
            let headers = String::from_utf8(headers).unwrap();
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            assert!(headers.starts_with("POST /api/v1/esplora/bitcoin/mainnet/tx HTTP/1.1\r\n"));
            assert!(!headers.to_ascii_lowercase().contains("authorization:"));
            assert_eq!(
                body,
                bitcoin::consensus::encode::serialize_hex(&tx).as_bytes()
            );
            let status = match case {
                1 => "401 Unauthorized",
                2 => "307 Temporary Redirect",
                _ => "200 OK",
            };
            let answer = match case {
                3 => Txid::all_zeros().to_string(),
                4 => "not a transaction id".into(),
                _ => tx.compute_txid().to_string(),
            };
            write!(
                stream,
                "HTTP/1.1 {}\r\nContent-Length: {}\r\nLocation: http://{}/redirected\r\nConnection: close\r\n\r\n{}",
                status,
                answer.len(),
                address,
                answer
            )
            .unwrap();
            drop(stream);
            let deadline = Instant::now() + Duration::from_millis(200);
            while Instant::now() < deadline {
                assert!(
                    untouched(&listener),
                    "sender retried or followed a redirect"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let (gate, revoker) = fresh_gate(&verified);
        let origin = format!("http://{}/", address);
        let result = submit_verified_split_step1_to_connect(&verified, &gate, &origin);
        let txid = verified.transaction().compute_txid();
        let wtxid = verified.transaction().compute_wtxid();
        if case == 0 {
            assert_eq!(
                result,
                Ok(SubmissionOutcome::UpstreamAccepted { txid, wtxid })
            );
        } else {
            assert_eq!(result, Err(SubmissionError::Uncertain { txid, wtxid }));
        }
        assert_eq!(revoker.state(), SubmissionState::Started);
        assert_eq!(
            submit_verified_split_step1_to_connect(&verified, &gate, &origin),
            Err(SubmissionError::AlreadyStarted)
        );
        worker.join().unwrap();
    }
}

/// (e) Every refusal happens before any connection: a malformed origin and a
/// gate of another transaction or chain leave the gate pending; an expired
/// deadline, or a revocation that wins the race to the gate, consumes it
/// without sending; a non-mainnet step 1 is refused outright.
#[test]
fn split_step1_connect_transport_refuses_before_http() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let verified = split_step1(ChainId::Bitcoin, 1);

    // Origin: exactly scheme://host[:port]/, like the Claim Connect route.
    let (gate, _revoker) = fresh_gate(&verified);
    for invalid in [
        format!("{}unexpected", origin),
        format!("{}?token=synthetic", origin),
        format!("{}#fragment", origin),
        format!(
            "http://synthetic:password@{}/",
            listener.local_addr().unwrap()
        ),
        "file:///tmp/synthetic".into(),
    ] {
        assert_eq!(
            submit_verified_split_step1_to_connect(&verified, &gate, &invalid),
            Err(SubmissionError::BackendUnavailable)
        );
        assert_eq!(gate.state(), SubmissionState::Pending);
    }

    // Gate of another step 1, or of another chain for the same bytes.
    let other = split_step1(ChainId::Bitcoin, 2);
    assert_ne!(
        other.transaction().compute_txid(),
        verified.transaction().compute_txid()
    );
    let (foreign, _) =
        SubmissionGate::for_split_step1(&other, Instant::now() + Duration::from_secs(30));
    let (wrong_chain, _) = SubmissionGate::for_transaction(
        ChainId::BitcoinBlake2b,
        verified.transaction(),
        Instant::now() + Duration::from_secs(30),
    );
    for gate in [&foreign, &wrong_chain] {
        assert_eq!(
            submit_verified_split_step1_to_connect(&verified, gate, &origin),
            Err(SubmissionError::GateMismatch)
        );
        assert_eq!(gate.state(), SubmissionState::Pending);
    }

    // A Testnet4 step 1 (a valid construction) has no Connect route yet.
    let testnet = split_step1(ChainId::Testnet4, 1);
    assert_eq!(testnet.chain(), ChainId::Testnet4);
    let (testnet_gate, _) = fresh_gate(&testnet);
    assert_eq!(
        submit_verified_split_step1_to_connect(&testnet, &testnet_gate, &origin),
        Err(SubmissionError::UnsupportedChain)
    );
    assert_eq!(testnet_gate.state(), SubmissionState::Pending);

    // Deadline already reached: refused at the gate and never reusable.
    let (expired, expired_revoker) = SubmissionGate::for_split_step1(&verified, Instant::now());
    assert_eq!(
        submit_verified_split_step1_to_connect(&verified, &expired, &origin),
        Err(SubmissionError::Expired)
    );
    assert_eq!(expired_revoker.state(), SubmissionState::Expired);
    assert_eq!(
        submit_verified_split_step1_to_connect(&verified, &expired, &origin),
        Err(SubmissionError::Expired)
    );

    // Revoked before the call.
    let (revoked, revoker) = fresh_gate(&verified);
    assert_eq!(revoker.revoke(), SubmissionState::Revoked);
    assert_eq!(
        submit_verified_split_step1_to_connect(&verified, &revoked, &origin),
        Err(SubmissionError::Revoked)
    );

    // Revoked after the request was prepared, before the gate is entered.
    let (mut racing, revoker) = fresh_gate(&verified);
    let barrier = Arc::new(Barrier::new(2));
    racing.before_lock = Some(barrier.clone());
    let worker = {
        let origin = origin.clone();
        std::thread::spawn(move || {
            submit_verified_split_step1_to_connect(&verified, &racing, &origin)
        })
    };
    barrier.wait();
    assert_eq!(revoker.revoke(), SubmissionState::Revoked);
    barrier.wait();
    assert_eq!(worker.join().unwrap(), Err(SubmissionError::Revoked));

    assert!(untouched(&listener));
}

/// The gate binds the signed transaction (txid and wtxid), not the unsigned
/// construction, and a Split gate is the same one-use gate as Claim's.
#[test]
fn split_step1_gate_binds_the_signed_witness_identity() {
    let verified = split_step1(ChainId::Bitcoin, 3);
    let (gate, revoker) = fresh_gate(&verified);
    assert_eq!(gate.chain, ChainId::Bitcoin);
    assert_eq!(gate.txid, verified.transaction().compute_txid());
    assert_eq!(gate.wtxid, verified.transaction().compute_wtxid());
    assert_ne!(gate.wtxid.to_byte_array(), gate.txid.to_byte_array());
    assert_eq!(gate.state(), SubmissionState::Pending);
    assert_eq!(revoker.revoke(), SubmissionState::Revoked);
    assert_eq!(gate.enter(), Err(SubmissionError::Revoked));
}

/// Not exposed over daemon RPC: neither the JSON-RPC dispatch nor any
/// `DaemonControl` command names the Split transport or its gate.
#[test]
fn split_step1_transport_is_not_exposed_over_rpc() {
    for (file, text) in [
        ("jsonrpc/api.rs", include_str!("../jsonrpc/api.rs")),
        ("jsonrpc/mod.rs", include_str!("../jsonrpc/mod.rs")),
        ("jsonrpc/rpc.rs", include_str!("../jsonrpc/rpc.rs")),
        ("commands/mod.rs", include_str!("../commands/mod.rs")),
        ("lib.rs", include_str!("../lib.rs")),
    ] {
        for ident in [
            "submit_verified_split_step1_to_connect",
            "for_split_step1",
            "VerifiedSplitStep1",
            "foreign_split",
        ] {
            assert!(!text.contains(ident), "{} names {}", file, ident);
        }
    }
}
