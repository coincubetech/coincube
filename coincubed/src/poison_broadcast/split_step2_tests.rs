//! Split (#568 B3b) step-2 transport tests. Step 2 is a real core
//! construction (`create_split_step2`) of a foreign single-key wallet's
//! claimed coins into the daemon's own Vault receive address, signed by
//! rust-bitcoin's reference PSBT signer and verified by
//! `finalize_split_step2`. The backend is the test double that records every
//! broadcast.
use super::*;
use crate::{
    config::{BitcoinConfig, Config},
    datadir::DataDirectory,
    testutils::{DummyBitcoind, DummyDatabase},
};
use coincube_core::{
    bip39::Mnemonic,
    claim::BlockRef,
    foreign_split::{
        create_split_step1, create_split_step2, create_unified_sweep, finalize_split_step2,
        finalize_unified_sweep, SplitBranch, SplitCoin, SplitInputs, SplitSource, SplitStep2Inputs,
        UnifiedInputs, UnifiedReplayStatus,
    },
    psbt_unified::UnifiedPsbt,
    signer::SessionSigner,
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
    str::FromStr,
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

const FORK: u64 = 900;
const TIP: u32 = 960;
/// The target Vault's receive index recorded in the Split journal.
const TARGET_INDEX: u32 = 3;
const VAULT: &str = "wsh(or_d(multi(2,[ffd63c8d/48'/1'/0'/2']tpubDExA3EC3iAsPxPhFn4j6gMiVup6V2eH3qKyk69RcTc9TTNRfFYVPad8bJD5FCHVQxyBT4izKsvr7Btd2R4xmQ1hZkvsqGBaeE82J71uTK4N/<0;1>/*,[de6eb005/48'/1'/0'/2']tpubDFGuYfS2JwiUSEXiQuNGdT3R7WTDhbaE6jbUhgYSSdhmfQcSx7ZntMPPv7nrkvAqjpj3jX9wbhSGMeKVao4qAzhbNyBi7iQmv5xxQk6H6jz/<0;1>/*),and_v(v:pkh([ffd63c8d/48'/1'/0'/2']tpubDExA3EC3iAsPxPhFn4j6gMiVup6V2eH3qKyk69RcTc9TTNRfFYVPad8bJD5FCHVQxyBT4izKsvr7Btd2R4xmQ1hZkvsqGBaeE82J71uTK4N/<2;3>/*),older(3))))#p9ax3xxp";

fn vault() -> CoincubeDescriptor {
    CoincubeDescriptor::from_str(VAULT).unwrap()
}

fn receive_script(descriptor: &CoincubeDescriptor, index: u32) -> bitcoin::ScriptBuf {
    descriptor
        .receive_descriptor()
        .derive(
            ChildNumber::from_normal_idx(index).unwrap(),
            &Secp256k1::verification_only(),
        )
        .script_pubkey()
}

/// A verified step 2 of a single-key `wpkh` foreign wallet (`seed` varies
/// it) paying `target`, on `chain`.
fn split_step2(chain: ChainId, seed: u8, target: &bitcoin::Script) -> VerifiedSplitStep2 {
    let secp = Secp256k1::new();
    let signer = Xpriv::new_master(Network::Bitcoin, &[seed; 32]).unwrap();
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
    let claimed = create_split_step1(
        &SplitInputs {
            chain: ChainId::Bitcoin,
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
    .unwrap()
    .claimed_prevouts();
    let step2 = create_split_step2(
        &SplitStep2Inputs {
            chain,
            source: &source,
            coins: &coins,
            fork_height: FORK,
            claimed: &claimed,
            target,
        },
        2,
        LockTime::from_height(TIP).unwrap(),
        TIP,
    )
    .unwrap();
    let mut psbt = step2.psbt().clone();
    psbt.sign(&signer, &secp).unwrap();
    finalize_split_step2(&step2, &coins, &source, &psbt, &secp).unwrap()
}

fn control(
    chain: ChainId,
    descriptor: CoincubeDescriptor,
    backend: Arc<Mutex<DummyBitcoind>>,
) -> DaemonControl {
    let config = Config::new(
        BitcoinConfig::new(chain, Duration::from_secs(2)),
        None,
        log::LevelFilter::Off,
        descriptor,
        DataDirectory::new(std::path::PathBuf::from("/synthetic-unused-split-step2")),
    );
    let (sender, _receiver) = mpsc::sync_channel(1);
    DaemonControl::new(
        config,
        backend,
        sender,
        Arc::new(Mutex::new(DummyDatabase::new())),
        bitcoin::secp256k1::Secp256k1::verification_only(),
        Default::default(),
        Default::default(),
    )
}

fn fresh_gate(verified: &VerifiedSplitStep2) -> (SubmissionGate, SubmissionRevoker) {
    SubmissionGate::for_split_step2(verified, Instant::now() + Duration::from_secs(60))
}

fn index(n: u32) -> ChildNumber {
    ChildNumber::from_normal_idx(n).unwrap()
}

fn sent(backend: &Arc<Mutex<DummyBitcoind>>) -> Vec<Transaction> {
    backend.lock().unwrap().broadcasted.lock().unwrap().clone()
}

/// The exact verified witness bytes, once, through the daemon's own backend,
/// when the one output is this Vault's receive address at the recorded
/// index. No descriptor check on the (foreign) inputs.
#[test]
fn split_step2_transport_sends_exact_bytes_once_to_the_reserved_vault_address() {
    let descriptor = vault();
    let verified = split_step2(
        ChainId::BitcoinBlake2b,
        1,
        &receive_script(&descriptor, TARGET_INDEX),
    );
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let daemon = control(ChainId::BitcoinBlake2b, descriptor, backend.clone());
    let (gate, _) = fresh_gate(&verified);
    assert_eq!(
        daemon.submit_verified_split_step2(
            &verified,
            index(TARGET_INDEX),
            &daemon.claim_backend_binding(),
            &gate
        ),
        Ok(SubmissionOutcome::UpstreamAccepted {
            txid: verified.transaction().compute_txid(),
            wtxid: verified.transaction().compute_wtxid(),
        })
    );
    assert_eq!(
        daemon.submit_verified_split_step2(
            &verified,
            index(TARGET_INDEX),
            &daemon.claim_backend_binding(),
            &gate
        ),
        Err(SubmissionError::AlreadyStarted)
    );
    let sent = sent(&backend);
    assert_eq!(sent.len(), 1);
    assert_eq!(
        bitcoin::consensus::serialize(&sent[0]),
        bitcoin::consensus::serialize(verified.transaction())
    );
}

/// Every refusal returns before the backend is called and leaves the gate
/// Pending: a wrong chain (daemon or artifact), an output that is not this
/// Vault's receive address at the recorded index (another index, the change
/// branch, another Vault, a hardened index), and a gate for another
/// transaction.
#[test]
fn split_step2_transport_refuses_wrong_chain_output_and_gate_before_sending() {
    let descriptor = vault();
    let target = receive_script(&descriptor, TARGET_INDEX);
    let verified = split_step2(ChainId::BitcoinBlake2b, 1, &target);
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));

    // Wrong daemon chain.
    for chain in [
        ChainId::Bitcoin,
        ChainId::Testnet4,
        ChainId::BitcoinBlake2bTestnet4,
    ] {
        let (gate, _) = fresh_gate(&verified);
        let other = control(chain, descriptor.clone(), backend.clone());
        assert_eq!(
            other.submit_verified_split_step2(
                &verified,
                index(TARGET_INDEX),
                &other.claim_backend_binding(),
                &gate
            ),
            Err(SubmissionError::UnsupportedChain),
            "{chain:?}"
        );
        assert_eq!(gate.state(), SubmissionState::Pending);
    }
    // Wrong artifact chain: a BTCB2 Testnet4 construction.
    let testnet = split_step2(ChainId::BitcoinBlake2bTestnet4, 1, &target);
    let (gate, _) = fresh_gate(&testnet);
    let other = control(ChainId::BitcoinBlake2b, descriptor.clone(), backend.clone());
    assert_eq!(
        other.submit_verified_split_step2(
            &testnet,
            index(TARGET_INDEX),
            &other.claim_backend_binding(),
            &gate
        ),
        Err(SubmissionError::UnsupportedChain)
    );

    let daemon = control(ChainId::BitcoinBlake2b, descriptor.clone(), backend.clone());
    // Another recorded index, and a hardened one.
    for wrong in [
        index(TARGET_INDEX + 1),
        index(TARGET_INDEX - 1),
        ChildNumber::from_hardened_idx(TARGET_INDEX).unwrap(),
    ] {
        let (gate, _) = fresh_gate(&verified);
        assert_eq!(
            daemon.submit_verified_split_step2(
                &verified,
                wrong,
                &daemon.claim_backend_binding(),
                &gate
            ),
            Err(SubmissionError::OutputMismatch),
            "{wrong}"
        );
        assert_eq!(gate.state(), SubmissionState::Pending);
    }
    // The change branch at the same index, and another Vault's address.
    let change = descriptor
        .change_descriptor()
        .derive(index(TARGET_INDEX), &Secp256k1::verification_only())
        .script_pubkey();
    let other = CoincubeDescriptor::from_str(
        &VAULT
            .split('#')
            .next()
            .unwrap()
            .replace("older(3)", "older(4)"),
    )
    .unwrap();
    for script in [change, receive_script(&other, TARGET_INDEX)] {
        let elsewhere = split_step2(ChainId::BitcoinBlake2b, 1, &script);
        let (gate, _) = fresh_gate(&elsewhere);
        assert_eq!(
            daemon.submit_verified_split_step2(
                &elsewhere,
                index(TARGET_INDEX),
                &daemon.claim_backend_binding(),
                &gate
            ),
            Err(SubmissionError::OutputMismatch)
        );
        assert_eq!(gate.state(), SubmissionState::Pending);
    }
    // A gate for another step 2 (another foreign wallet, same target).
    let other_step2 = split_step2(ChainId::BitcoinBlake2b, 2, &target);
    assert_ne!(
        other_step2.transaction().compute_txid(),
        verified.transaction().compute_txid()
    );
    let (gate, _) = fresh_gate(&other_step2);
    assert_eq!(
        daemon.submit_verified_split_step2(
            &verified,
            index(TARGET_INDEX),
            &daemon.claim_backend_binding(),
            &gate
        ),
        Err(SubmissionError::GateMismatch)
    );
    assert_eq!(gate.state(), SubmissionState::Pending);
    // A Claim fork gate is a different constructor for a different artifact;
    // the step-1 gate of a Split cannot carry step 2 either: its chain is
    // Bitcoin. (Covered by GateMismatch on chain below.)
    let (wrong_chain_gate, _) = SubmissionGate::for_transaction(
        ChainId::Bitcoin,
        verified.transaction(),
        Instant::now() + Duration::from_secs(60),
    );
    assert_eq!(
        daemon.submit_verified_split_step2(
            &verified,
            index(TARGET_INDEX),
            &daemon.claim_backend_binding(),
            &wrong_chain_gate
        ),
        Err(SubmissionError::GateMismatch)
    );
    assert!(sent(&backend).is_empty());
}

/// Revocation and expiry win before the backend; a lost response after the
/// gate was entered is Uncertain, and the gate cannot be reused.
#[test]
fn split_step2_transport_revocation_expiry_and_uncertain_response() {
    let descriptor = vault();
    let verified = split_step2(
        ChainId::BitcoinBlake2b,
        1,
        &receive_script(&descriptor, TARGET_INDEX),
    );
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let daemon = control(ChainId::BitcoinBlake2b, descriptor, backend.clone());
    let (revoked, revoker) = fresh_gate(&verified);
    assert_eq!(revoker.revoke(), SubmissionState::Revoked);
    assert_eq!(
        daemon.submit_verified_split_step2(
            &verified,
            index(TARGET_INDEX),
            &daemon.claim_backend_binding(),
            &revoked
        ),
        Err(SubmissionError::Revoked)
    );
    let (expired, _) = SubmissionGate::for_split_step2(&verified, Instant::now());
    assert_eq!(
        daemon.submit_verified_split_step2(
            &verified,
            index(TARGET_INDEX),
            &daemon.claim_backend_binding(),
            &expired
        ),
        Err(SubmissionError::Expired)
    );
    assert!(sent(&backend).is_empty());
    backend.lock().unwrap().broadcast_error = Some("response lost".into());
    let (gate, _) = fresh_gate(&verified);
    assert_eq!(
        daemon.submit_verified_split_step2(
            &verified,
            index(TARGET_INDEX),
            &daemon.claim_backend_binding(),
            &gate
        ),
        Err(SubmissionError::Uncertain {
            txid: verified.transaction().compute_txid(),
            wtxid: verified.transaction().compute_wtxid(),
        })
    );
    assert_eq!(gate.state(), SubmissionState::Started);
    assert_eq!(sent(&backend).len(), 1);
}

/// #568 B3b-2: the Connect route refuses a backend switched since review.
/// A binding captured from another daemon instance (a restart or a backend
/// switch replaces the controller) refuses before the backend is called and
/// leaves the gate Pending; the current binding then sends.
#[test]
fn split_step2_connect_route_refuses_a_switched_backend() {
    let descriptor = vault();
    let verified = split_step2(
        ChainId::BitcoinBlake2b,
        1,
        &receive_script(&descriptor, TARGET_INDEX),
    );
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let reviewed = control(ChainId::BitcoinBlake2b, descriptor.clone(), backend.clone())
        .claim_backend_binding();
    let daemon = control(
        ChainId::BitcoinBlake2b,
        descriptor,
        Arc::new(Mutex::new(DummyBitcoind::new())),
    );
    let (gate, _) = fresh_gate(&verified);
    assert_eq!(
        daemon.submit_verified_split_step2(&verified, index(TARGET_INDEX), &reviewed, &gate),
        Err(SubmissionError::BackendUnavailable)
    );
    assert_eq!(gate.state(), SubmissionState::Pending);
    assert_eq!(
        daemon.submit_verified_split_step2(
            &verified,
            index(TARGET_INDEX),
            &daemon.claim_backend_binding(),
            &gate
        ),
        Ok(SubmissionOutcome::UpstreamAccepted {
            txid: verified.transaction().compute_txid(),
            wtxid: verified.transaction().compute_wtxid(),
        })
    );
    assert!(sent(&backend).is_empty());
}

/// P4: the managed-node route refuses an Esplora-backed daemon (no bound
/// node) and a stale binding before any request, after the same output
/// checks.
#[test]
fn split_step2_node_route_needs_a_bound_node_and_a_current_binding() {
    let descriptor = vault();
    let verified = split_step2(
        ChainId::BitcoinBlake2b,
        1,
        &receive_script(&descriptor, TARGET_INDEX),
    );
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let daemon = control(ChainId::BitcoinBlake2b, descriptor.clone(), backend.clone());
    // No Bitcoind backend in this config.
    let binding = daemon.claim_backend_binding();
    let (gate, _) = fresh_gate(&verified);
    assert_eq!(
        daemon.submit_verified_split_step2_to_node(&verified, index(TARGET_INDEX), &binding, &gate),
        Err(SubmissionError::BackendUnavailable)
    );
    assert_eq!(gate.state(), SubmissionState::Pending);
    // A binding captured from another daemon instance.
    let other = control(ChainId::BitcoinBlake2b, descriptor, backend.clone());
    let stale = other.claim_backend_binding();
    assert_eq!(
        daemon.submit_verified_split_step2_to_node(&verified, index(TARGET_INDEX), &stale, &gate),
        Err(SubmissionError::BackendUnavailable)
    );
    // The output check runs first.
    assert_eq!(
        daemon.submit_verified_split_step2_to_node(
            &verified,
            index(TARGET_INDEX + 1),
            &binding,
            &gate
        ),
        Err(SubmissionError::OutputMismatch)
    );
    assert_eq!(gate.state(), SubmissionState::Pending);
    assert!(sent(&backend).is_empty());
}

/// Not exposed over daemon RPC.
#[test]
fn split_step2_transport_is_not_exposed_over_rpc() {
    for (file, text) in [
        ("jsonrpc/api.rs", include_str!("../jsonrpc/api.rs")),
        ("jsonrpc/mod.rs", include_str!("../jsonrpc/mod.rs")),
        ("jsonrpc/rpc.rs", include_str!("../jsonrpc/rpc.rs")),
        ("commands/mod.rs", include_str!("../commands/mod.rs")),
        ("lib.rs", include_str!("../lib.rs")),
    ] {
        for ident in [
            "submit_verified_split_step2",
            "for_split_step2",
            "VerifiedSplitStep2",
            "foreign_split",
        ] {
            assert!(!text.contains(ident), "{} names {}", file, ident);
        }
    }
}

/// One `sendrawtransaction` received by a local fake Knots node: returns the
/// request body; answers with `reply_txid`.
fn fake_node(
    reply_txid: Txid,
) -> (
    std::net::SocketAddr,
    std::thread::JoinHandle<serde_json::Value>,
) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut headers = Vec::new();
        let mut byte = [0];
        while !headers.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            headers.push(byte[0]);
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
        let reply = serde_json::json!({"id":1,"result":reply_txid,"error":null}).to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            reply.len(),
            reply
        )
        .unwrap();
        serde_json::from_slice(&body).unwrap()
    });
    (address, worker)
}

fn node_control(descriptor: CoincubeDescriptor, address: std::net::SocketAddr) -> DaemonControl {
    use crate::config::{BitcoinBackend, BitcoindConfig, BitcoindRpcAuth};
    let config = Config::new(
        BitcoinConfig::new(ChainId::BitcoinBlake2b, Duration::from_secs(2)),
        Some(BitcoinBackend::Bitcoind(BitcoindConfig {
            addr: address,
            rpc_auth: BitcoindRpcAuth::UserPass("synthetic".into(), "fixture".into()),
        })),
        log::LevelFilter::Off,
        descriptor,
        DataDirectory::new(std::path::PathBuf::from("/synthetic-unused-split-step2")),
    );
    let (sender, _receiver) = mpsc::sync_channel(1);
    DaemonControl::new(
        config,
        Arc::new(Mutex::new(DummyBitcoind::new())),
        sender,
        Arc::new(Mutex::new(DummyDatabase::new())),
        bitcoin::secp256k1::Secp256k1::verification_only(),
        Default::default(),
        Default::default(),
    )
}

/// #568 B3b-2, P4 end to end at the transport: the bound Knots node receives
/// exactly one `sendrawtransaction` of the verified witness bytes, through the
/// binding captured at review. A binding from another daemon instance (a
/// node switched since review) refuses before any connection.
#[test]
fn split_step2_node_route_sends_exact_bytes_once_to_the_bound_node() {
    let descriptor = vault();
    let verified = split_step2(
        ChainId::BitcoinBlake2b,
        1,
        &receive_script(&descriptor, TARGET_INDEX),
    );
    let tx = verified.transaction().clone();
    let (address, worker) = fake_node(tx.compute_txid());
    let daemon = node_control(descriptor.clone(), address);
    // A switched node: the binding of another controller at the same address.
    let stale = node_control(descriptor, address).claim_backend_binding();
    let (gate, _) = fresh_gate(&verified);
    assert_eq!(
        daemon.submit_verified_split_step2_to_node(&verified, index(TARGET_INDEX), &stale, &gate),
        Err(SubmissionError::BackendUnavailable)
    );
    assert_eq!(gate.state(), SubmissionState::Pending);
    let binding = daemon.claim_backend_binding();
    assert_eq!(
        daemon.submit_verified_split_step2_to_node(&verified, index(TARGET_INDEX), &binding, &gate),
        Ok(SubmissionOutcome::UpstreamAccepted {
            txid: tx.compute_txid(),
            wtxid: tx.compute_wtxid(),
        })
    );
    assert_eq!(
        worker.join().unwrap(),
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"sendrawtransaction","params":[bitcoin::consensus::encode::serialize_hex(&tx)]})
    );
    assert_eq!(
        daemon.submit_verified_split_step2_to_node(&verified, index(TARGET_INDEX), &binding, &gate),
        Err(SubmissionError::AlreadyStarted)
    );
}

/// A verified unified sweep (#568 B4b) of a single-key `wpkh` foreign
/// wallet (`seed` varies it) paying `target`, on `chain`, signed
/// `ALL|UNIFIED` by the session signer and finalized Protected.
fn unified_sweep(chain: ChainId, seed: u8, target: &bitcoin::Script) -> VerifiedUnifiedSweep {
    let secp = Secp256k1::new();
    let signer = SessionSigner::from_mnemonic(
        Network::Bitcoin,
        Mnemonic::from_entropy(&[seed; 16]).unwrap(),
        "",
    )
    .unwrap();
    let path = DerivationPath::from_str("m/84'/0'/0'").unwrap();
    let key = format!(
        "[{}/84'/0'/0']{}",
        signer.fingerprint(&secp),
        signer.xpub_at(&path, &secp)
    );
    let branch = |b: u32| Descriptor::from_str(&format!("wpkh({}/{}/*)", key, b)).unwrap();
    let source = SplitSource::new(branch(0), Some(branch(1))).unwrap();
    let previous = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([seed; 32]), 1),
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
    let sweep = create_unified_sweep(
        &UnifiedInputs {
            chain,
            source: &source,
            coins: &coins,
            fork_height: FORK,
            target,
        },
        2,
        LockTime::from_height(TIP).unwrap(),
        TIP,
    )
    .unwrap();
    let signed = signer
        .sign_unified(
            &UnifiedPsbt::from_psbt(sweep.psbt().clone()).unwrap(),
            chain,
            &secp,
        )
        .unwrap();
    finalize_unified_sweep(&sweep, &signed, &secp).unwrap()
}

fn unified_gate(verified: &VerifiedUnifiedSweep) -> (SubmissionGate, SubmissionRevoker) {
    SubmissionGate::for_unified_sweep(verified, Instant::now() + Duration::from_secs(60))
}

/// #568 B4b: the unified sweep takes step 2's transport and refusals, on
/// both routes. Every refusal returns before any backend or node is touched
/// and leaves the gate Pending: a wrong chain (daemon or artifact), an
/// output that is not this Vault's receive address at the recorded index
/// (another index, a hardened one, the change branch, another Vault), and a
/// gate for another transaction, another chain or a step-2 artifact. Then
/// the exact bytes go once through the backend route and once through the
/// bound node route, and neither method is reachable over RPC.
#[test]
fn unified_transport_refuses_wrong_chain_output_and_gate() {
    let descriptor = vault();
    let target = receive_script(&descriptor, TARGET_INDEX);
    let verified = unified_sweep(ChainId::BitcoinBlake2b, 1, &target);
    assert_eq!(verified.replay_status(), UnifiedReplayStatus::Protected);
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let both = |daemon: &DaemonControl,
                verified: &VerifiedUnifiedSweep,
                target_index: ChildNumber,
                gate: &SubmissionGate| {
        let binding = daemon.claim_backend_binding();
        let backend = daemon.submit_verified_unified_sweep(verified, target_index, &binding, gate);
        let node =
            daemon.submit_verified_unified_sweep_to_node(verified, target_index, &binding, gate);
        assert_eq!(backend, node);
        assert_eq!(gate.state(), SubmissionState::Pending);
        backend
    };

    // Wrong daemon chain.
    for chain in [
        ChainId::Bitcoin,
        ChainId::Testnet4,
        ChainId::BitcoinBlake2bTestnet4,
    ] {
        let (gate, _) = unified_gate(&verified);
        let other = control(chain, descriptor.clone(), backend.clone());
        assert_eq!(
            both(&other, &verified, index(TARGET_INDEX), &gate),
            Err(SubmissionError::UnsupportedChain),
            "{chain:?}"
        );
    }
    // Wrong artifact chain: a BTCB2 Testnet4 construction.
    let testnet = unified_sweep(ChainId::BitcoinBlake2bTestnet4, 1, &target);
    let (gate, _) = unified_gate(&testnet);
    let daemon = control(ChainId::BitcoinBlake2b, descriptor.clone(), backend.clone());
    assert_eq!(
        both(&daemon, &testnet, index(TARGET_INDEX), &gate),
        Err(SubmissionError::UnsupportedChain)
    );
    // Another recorded index, and a hardened one.
    for wrong in [
        index(TARGET_INDEX + 1),
        index(TARGET_INDEX - 1),
        ChildNumber::from_hardened_idx(TARGET_INDEX).unwrap(),
    ] {
        let (gate, _) = unified_gate(&verified);
        assert_eq!(
            both(&daemon, &verified, wrong, &gate),
            Err(SubmissionError::OutputMismatch),
            "{wrong}"
        );
    }
    // The change branch at the same index, and another Vault's address.
    let change = descriptor
        .change_descriptor()
        .derive(index(TARGET_INDEX), &Secp256k1::verification_only())
        .script_pubkey();
    let other_vault = CoincubeDescriptor::from_str(
        &VAULT
            .split('#')
            .next()
            .unwrap()
            .replace("older(3)", "older(4)"),
    )
    .unwrap();
    for script in [change, receive_script(&other_vault, TARGET_INDEX)] {
        let elsewhere = unified_sweep(ChainId::BitcoinBlake2b, 1, &script);
        let (gate, _) = unified_gate(&elsewhere);
        assert_eq!(
            both(&daemon, &elsewhere, index(TARGET_INDEX), &gate),
            Err(SubmissionError::OutputMismatch)
        );
    }
    // A gate for another sweep (another foreign wallet, same target), a gate
    // on the Bitcoin chain, and a step-2 gate (another artifact).
    let other_sweep = unified_sweep(ChainId::BitcoinBlake2b, 2, &target);
    assert_ne!(
        other_sweep.transaction().compute_txid(),
        verified.transaction().compute_txid()
    );
    let (gate, _) = unified_gate(&other_sweep);
    assert_eq!(
        both(&daemon, &verified, index(TARGET_INDEX), &gate),
        Err(SubmissionError::GateMismatch)
    );
    let (wrong_chain_gate, _) = SubmissionGate::for_transaction(
        ChainId::Bitcoin,
        verified.transaction(),
        Instant::now() + Duration::from_secs(60),
    );
    assert_eq!(
        both(&daemon, &verified, index(TARGET_INDEX), &wrong_chain_gate),
        Err(SubmissionError::GateMismatch)
    );
    let (step2_gate, _) = fresh_gate(&split_step2(ChainId::BitcoinBlake2b, 1, &target));
    assert_eq!(
        both(&daemon, &verified, index(TARGET_INDEX), &step2_gate),
        Err(SubmissionError::GateMismatch)
    );
    assert!(sent(&backend).is_empty());

    // The exact bytes, once, through the backend route.
    let (gate, _) = unified_gate(&verified);
    let binding = daemon.claim_backend_binding();
    assert_eq!(
        daemon.submit_verified_unified_sweep(&verified, index(TARGET_INDEX), &binding, &gate),
        Ok(SubmissionOutcome::UpstreamAccepted {
            txid: verified.transaction().compute_txid(),
            wtxid: verified.transaction().compute_wtxid(),
        })
    );
    assert_eq!(
        daemon.submit_verified_unified_sweep(&verified, index(TARGET_INDEX), &binding, &gate),
        Err(SubmissionError::AlreadyStarted)
    );
    let sent = sent(&backend);
    assert_eq!(sent.len(), 1);
    assert_eq!(
        bitcoin::consensus::serialize(&sent[0]),
        bitcoin::consensus::serialize(verified.transaction())
    );

    // The exact bytes, once, through the bound node route; a binding from
    // another daemon instance refuses before any connection.
    let tx = verified.transaction().clone();
    let (address, worker) = fake_node(tx.compute_txid());
    let node = node_control(descriptor.clone(), address);
    let stale = node_control(descriptor, address).claim_backend_binding();
    let (gate, _) = unified_gate(&verified);
    assert_eq!(
        node.submit_verified_unified_sweep_to_node(&verified, index(TARGET_INDEX), &stale, &gate),
        Err(SubmissionError::BackendUnavailable)
    );
    assert_eq!(gate.state(), SubmissionState::Pending);
    let binding = node.claim_backend_binding();
    assert_eq!(
        node.submit_verified_unified_sweep_to_node(&verified, index(TARGET_INDEX), &binding, &gate),
        Ok(SubmissionOutcome::UpstreamAccepted {
            txid: tx.compute_txid(),
            wtxid: tx.compute_wtxid(),
        })
    );
    assert_eq!(
        worker.join().unwrap(),
        serde_json::json!({"jsonrpc":"2.0","id":1,"method":"sendrawtransaction","params":[bitcoin::consensus::encode::serialize_hex(&tx)]})
    );
    assert_eq!(
        node.submit_verified_unified_sweep_to_node(&verified, index(TARGET_INDEX), &binding, &gate),
        Err(SubmissionError::AlreadyStarted)
    );

    // Not exposed over RPC.
    for (file, text) in [
        ("jsonrpc/api.rs", include_str!("../jsonrpc/api.rs")),
        ("jsonrpc/mod.rs", include_str!("../jsonrpc/mod.rs")),
        ("jsonrpc/rpc.rs", include_str!("../jsonrpc/rpc.rs")),
        ("commands/mod.rs", include_str!("../commands/mod.rs")),
        ("lib.rs", include_str!("../lib.rs")),
    ] {
        for ident in [
            "submit_verified_unified_sweep",
            "for_unified_sweep",
            "VerifiedUnifiedSweep",
        ] {
            assert!(!text.contains(ident), "{} names {}", file, ident);
        }
    }
}
