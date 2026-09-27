use super::*;
use crate::{
    config::{BitcoinConfig, Config},
    datadir::DataDirectory,
    testutils::{DummyBitcoind, DummyDatabase},
};
use coincube_core::{
    claim_finalize::finalize_poison_transfer,
    claim_spend::{create_poison_self_transfer, PoisonSelfTransfer},
    descriptors::{CoincubeDescriptor, CoincubePolicy, PathInfo},
    signer::MasterSigner,
    spend::{CandidateCoin, TxGetter},
};
use miniscript::{
    bitcoin::{
        self, absolute,
        bip32::{ChildNumber, DerivationPath},
        hashes::Hash,
        psbt::Psbt,
        secp256k1, Amount, BlockHash, OutPoint, Sequence, Transaction, TxIn, TxOut,
    },
    DescriptorPublicKey,
};
use std::{
    collections::HashMap,
    str::FromStr,
    sync::{mpsc, Arc, Mutex},
    time::Duration,
};
fn signer(byte: u8) -> MasterSigner {
    MasterSigner::from_mnemonic(
        bitcoin::Network::Bitcoin,
        coincube_core::bip39::Mnemonic::from_entropy(&[byte; 16]).unwrap(),
    )
    .unwrap()
}
struct Getter(HashMap<Txid, Transaction>);
impl TxGetter for Getter {
    fn get_tx(&mut self, id: &Txid) -> Option<Transaction> {
        self.0.get(id).cloned()
    }
}
fn fixture_inputs(
    recovery: bool,
) -> (
    CoincubeDescriptor,
    Getter,
    Vec<CandidateCoin>,
    Vec<MasterSigner>,
) {
    let secp = secp256k1::Secp256k1::new();
    let signers: Vec<_> = (10..14).map(signer).collect();
    let keys: Vec<_> = signers
        .iter()
        .map(|s| {
            DescriptorPublicKey::from_str(&format!(
                "[{}]{}/<0;1>/*",
                s.fingerprint(&secp),
                s.xpub_at(&DerivationPath::default(), &secp)
            ))
            .unwrap()
        })
        .collect();
    let descriptor = CoincubeDescriptor::new(
        CoincubePolicy::new_legacy(
            PathInfo::Multi(2, keys[..3].to_vec()),
            std::iter::once((46, PathInfo::Single(keys[3].clone()))).collect(),
        )
        .unwrap(),
    );
    let verify = secp256k1::Secp256k1::verification_only();
    let mut getter = Getter(HashMap::new());
    let mut coins = Vec::new();
    for index in 7..9 {
        let index = ChildNumber::from_normal_idx(index).unwrap();
        let previous = Transaction {
            version: bitcoin::transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(100_000),
                script_pubkey: descriptor
                    .receive_descriptor()
                    .derive(index, &verify)
                    .script_pubkey(),
            }],
        };
        coins.push(CandidateCoin {
            outpoint: OutPoint::new(previous.compute_txid(), 0),
            amount: previous.output[0].value,
            deriv_index: index,
            is_change: false,
            must_select: true,
            sequence: recovery.then_some(Sequence(46)),
            ancestor_info: None,
        });
        getter.0.insert(previous.compute_txid(), previous);
    }
    (descriptor, getter, coins, signers)
}
fn fixture(chain: ChainId, recovery: bool) -> (PoisonSelfTransfer, Vec<MasterSigner>) {
    let (descriptor, mut getter, coins, signers) = fixture_inputs(recovery);
    let verify = secp256k1::Secp256k1::verification_only();
    let built = create_poison_self_transfer(
        chain,
        &descriptor,
        &verify,
        &mut getter,
        &coins,
        ChildNumber::from_normal_idx(12).unwrap(),
        5,
        absolute::LockTime::ZERO,
        BlockHash::from_byte_array([42; 32]),
    )
    .unwrap();
    (built, signers)
}
fn sign(built: &PoisonSelfTransfer, signers: &[MasterSigner]) -> Psbt {
    let secp = secp256k1::Secp256k1::new();
    signers.iter().fold(built.psbt().clone(), |psbt, s| {
        s.sign_psbt(psbt, &secp).unwrap()
    })
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
        DataDirectory::new(std::path::PathBuf::from("/synthetic-unused-poison")),
    );
    let (sender, _receiver) = mpsc::sync_channel(1);
    DaemonControl::new(
        config,
        backend,
        sender,
        Arc::new(Mutex::new(DummyDatabase::new())),
        secp256k1::Secp256k1::verification_only(),
        Default::default(),
        Default::default(),
    )
}
#[test]
fn exact_final_witness_bytes_are_submitted_once_without_database_or_poller() {
    let (built, signers) = fixture(ChainId::Bitcoin, false);
    let signed = sign(&built, &signers[..2]);
    let verified =
        finalize_poison_transfer(&built, &signed, &secp256k1::Secp256k1::verification_only())
            .unwrap();
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let daemon = control(
        ChainId::Bitcoin,
        built.descriptor().clone(),
        backend.clone(),
    );
    let txid = verified.transaction().compute_txid();
    let wtxid = verified.transaction().compute_wtxid();
    assert_ne!(txid.to_string(), wtxid.to_string());
    assert_eq!(
        daemon.submit_verified_poison(
            &verified,
            &SubmissionGate::new(
                &verified,
                std::time::Instant::now() + Duration::from_secs(60)
            )
            .0
        ),
        Ok(SubmissionOutcome::UpstreamAccepted { txid, wtxid })
    );
    let backend = backend.lock().unwrap();
    assert_eq!(backend.broadcasted.lock().unwrap().len(), 1);
    assert_eq!(
        bitcoin::consensus::serialize(&backend.broadcasted.lock().unwrap()[0]),
        bitcoin::consensus::serialize(verified.transaction())
    );
    assert_eq!(
        backend.broadcasted.lock().unwrap()[0].compute_wtxid(),
        wtxid
    );
}
#[test]
fn binding_refusals_make_no_call_and_transport_failure_is_uncertain() {
    let (built, signers) = fixture(ChainId::Bitcoin, false);
    let signed = sign(&built, &signers[..2]);
    let secp = secp256k1::Secp256k1::verification_only();
    let verified = finalize_poison_transfer(&built, &signed, &secp).unwrap();
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    for chain in [ChainId::BitcoinBlake2b, ChainId::Testnet4, ChainId::Testnet] {
        assert_eq!(
            control(chain, built.descriptor().clone(), backend.clone()).submit_verified_poison(
                &verified,
                &SubmissionGate::new(
                    &verified,
                    std::time::Instant::now() + Duration::from_secs(60)
                )
                .0
            ),
            Err(SubmissionError::UnsupportedChain)
        );
    }
    let (testnet, signers) = fixture(ChainId::Testnet4, false);
    let testnet =
        finalize_poison_transfer(&testnet, &sign(&testnet, &signers[..2]), &secp).unwrap();
    assert_eq!(
        control(
            ChainId::Bitcoin,
            built.descriptor().clone(),
            backend.clone()
        )
        .submit_verified_poison(
            &testnet,
            &SubmissionGate::new(
                &testnet,
                std::time::Instant::now() + Duration::from_secs(60)
            )
            .0
        ),
        Err(SubmissionError::UnsupportedChain)
    );
    // A structurally valid but different recovery delay is a different wallet.
    let other = CoincubeDescriptor::from_str(
        &built
            .descriptor()
            .to_string()
            .split('#')
            .next()
            .unwrap()
            .replace("older(46)", "older(47)"),
    )
    .unwrap();
    assert_ne!(&other, built.descriptor());
    assert_eq!(
        control(ChainId::Bitcoin, other, backend.clone()).submit_verified_poison(
            &verified,
            &SubmissionGate::new(
                &verified,
                std::time::Instant::now() + Duration::from_secs(60)
            )
            .0
        ),
        Err(SubmissionError::DescriptorMismatch)
    );
    assert!(backend
        .lock()
        .unwrap()
        .broadcasted
        .lock()
        .unwrap()
        .is_empty());
    backend.lock().unwrap().broadcast_error =
        Some("synthetic response lost after submission".into());
    let daemon = control(
        ChainId::Bitcoin,
        built.descriptor().clone(),
        backend.clone(),
    );
    assert_eq!(
        daemon.submit_verified_poison(
            &verified,
            &SubmissionGate::new(
                &verified,
                std::time::Instant::now() + Duration::from_secs(60)
            )
            .0
        ),
        Err(SubmissionError::Uncertain {
            txid: verified.transaction().compute_txid(),
            wtxid: verified.transaction().compute_wtxid()
        })
    );
    assert_eq!(backend.lock().unwrap().broadcasted.lock().unwrap().len(), 1); // no retry
}

#[test]
fn revocation_while_waiting_for_backend_lock_prevents_submission() {
    let (built, signers) = fixture(ChainId::Bitcoin, false);
    let verified = finalize_poison_transfer(
        &built,
        &sign(&built, &signers[..2]),
        &secp256k1::Secp256k1::verification_only(),
    )
    .unwrap();
    let (mut gate, revoker) = SubmissionGate::new(
        &verified,
        std::time::Instant::now() + Duration::from_secs(60),
    );
    let barrier = Arc::new(std::sync::Barrier::new(2));
    gate.before_lock = Some(barrier.clone()); // test-only rendezvous before lock acquisition
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let daemon = control(
        ChainId::Bitcoin,
        built.descriptor().clone(),
        backend.clone(),
    );
    let locked = backend.lock().unwrap();
    let thread = std::thread::spawn(move || daemon.submit_verified_poison(&verified, &gate));
    barrier.wait();
    assert_eq!(revoker.clone().revoke(), SubmissionState::Revoked);
    assert!(locked.broadcasted.lock().unwrap().is_empty());
    drop(locked);
    assert_eq!(thread.join().unwrap(), Err(SubmissionError::Revoked));
    assert!(backend
        .lock()
        .unwrap()
        .broadcasted
        .lock()
        .unwrap()
        .is_empty());
}
#[test]
fn gates_bind_witness_identity_and_cannot_be_reused_or_reset() {
    let (built, signers) = fixture(ChainId::Bitcoin, false);
    let secp = secp256k1::Secp256k1::verification_only();
    let verified = finalize_poison_transfer(&built, &sign(&built, &signers[..2]), &secp).unwrap();
    let alternate = finalize_poison_transfer(&built, &sign(&built, &signers[1..3]), &secp).unwrap();
    assert_eq!(
        verified.transaction().compute_txid(),
        alternate.transaction().compute_txid()
    );
    assert_ne!(
        verified.transaction().compute_wtxid(),
        alternate.transaction().compute_wtxid()
    );
    let (gate, revoker) = SubmissionGate::new(
        &verified,
        std::time::Instant::now() + Duration::from_secs(60),
    );
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let daemon = control(
        ChainId::Bitcoin,
        built.descriptor().clone(),
        backend.clone(),
    );
    assert_eq!(
        daemon.submit_verified_poison(&alternate, &gate),
        Err(SubmissionError::GateMismatch)
    );
    assert_eq!(gate.state(), SubmissionState::Pending);
    assert!(daemon.submit_verified_poison(&verified, &gate).is_ok());
    assert_eq!(revoker.revoke(), SubmissionState::Started);
    assert_eq!(revoker.state(), SubmissionState::Started);
    assert_eq!(
        daemon.submit_verified_poison(&verified, &gate),
        Err(SubmissionError::AlreadyStarted)
    );
    assert_eq!(backend.lock().unwrap().broadcasted.lock().unwrap().len(), 1);
}

#[test]
fn poisoned_backend_lock_refuses_without_consuming_gate_or_calling_transport() {
    let (built, signers) = fixture(ChainId::Bitcoin, false);
    let verified = finalize_poison_transfer(
        &built,
        &sign(&built, &signers[..2]),
        &secp256k1::Secp256k1::verification_only(),
    )
    .unwrap();
    let (gate, _) = SubmissionGate::new(
        &verified,
        std::time::Instant::now() + Duration::from_secs(60),
    );
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let poisoned = backend.clone();
    assert!(std::thread::spawn(move || {
        let _guard = poisoned.lock().unwrap();
        panic!("synthetic poisoned mutex");
    })
    .join()
    .is_err());
    let daemon = control(
        ChainId::Bitcoin,
        built.descriptor().clone(),
        backend.clone(),
    );
    assert_eq!(
        daemon.submit_verified_poison(&verified, &gate),
        Err(SubmissionError::BackendUnavailable)
    );
    assert_eq!(gate.state(), SubmissionState::Pending);
    assert!(backend
        .lock()
        .err()
        .unwrap()
        .into_inner()
        .broadcasted
        .lock()
        .unwrap()
        .is_empty());
}

#[test]
fn deadline_expiring_behind_backend_lock_prevents_submission_and_reuse() {
    let (built, signers) = fixture(ChainId::Bitcoin, false);
    let verified = Arc::new(
        finalize_poison_transfer(
            &built,
            &sign(&built, &signers[..2]),
            &secp256k1::Secp256k1::verification_only(),
        )
        .unwrap(),
    );
    let deadline = std::time::Instant::now() + Duration::from_millis(50);
    let (mut gate, revoker) = SubmissionGate::new(&verified, deadline);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    gate.before_lock = Some(barrier.clone());
    let gate = Arc::new(gate);
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let daemon = control(
        ChainId::Bitcoin,
        built.descriptor().clone(),
        backend.clone(),
    );
    let locked = backend.lock().unwrap();
    let worker = daemon.clone();
    let worker_gate = gate.clone();
    let worker_tx = verified.clone();
    let task = std::thread::spawn(move || worker.submit_verified_poison(&worker_tx, &worker_gate));
    barrier.wait();
    std::thread::sleep(deadline.saturating_duration_since(std::time::Instant::now()));
    drop(locked);
    assert_eq!(task.join().unwrap(), Err(SubmissionError::Expired));
    assert_eq!(gate.state(), SubmissionState::Expired);
    assert_eq!(revoker.revoke(), SubmissionState::Expired);
    assert!(backend
        .lock()
        .unwrap()
        .broadcasted
        .lock()
        .unwrap()
        .is_empty());
    // Exercise one-use terminal expiry without the test-only lock rendezvous.
    assert_eq!(gate.enter(), Err(SubmissionError::Expired));
}

fn fork_fixture(
    chain: ChainId,
) -> (
    coincube_core::claim_spend::ClaimForkSweep,
    Vec<MasterSigner>,
) {
    let (source, signers) = fixture(chain, false);
    let mut getter = Getter(HashMap::new());
    let coins: Vec<_> = source
        .psbt()
        .inputs
        .iter()
        .enumerate()
        .map(|(i, input)| {
            let previous = input.non_witness_utxo.clone().unwrap();
            getter.0.insert(previous.compute_txid(), previous);
            CandidateCoin {
                outpoint: source.psbt().unsigned_tx.input[i].previous_output,
                amount: input.witness_utxo.as_ref().unwrap().value,
                deriv_index: ChildNumber::from_normal_idx(7 + i as u32).unwrap(),
                is_change: false,
                must_select: false,
                sequence: None,
                ancestor_info: None,
            }
        })
        .collect();
    let fork = if chain == ChainId::Bitcoin {
        ChainId::BitcoinBlake2b
    } else {
        ChainId::BitcoinBlake2bTestnet4
    };
    let sweep = coincube_core::claim_spend::create_claim_fork_sweep(
        &source,
        fork,
        &secp256k1::Secp256k1::verification_only(),
        &mut getter,
        &coins,
        ChildNumber::from_normal_idx(20).unwrap(),
        3,
        absolute::LockTime::ZERO,
    )
    .unwrap();
    (sweep, signers)
}

fn verified_fork(chain: ChainId, signer_indices: &[usize]) -> VerifiedClaimForkSweep {
    let (sweep, signers) = fork_fixture(chain);
    let secp = secp256k1::Secp256k1::new();
    let signed = signer_indices.iter().fold(sweep.psbt().clone(), |psbt, i| {
        signers[*i].sign_psbt(psbt, &secp).unwrap()
    });
    coincube_core::claim_finalize::finalize_claim_fork_sweep(
        &sweep,
        &coincube_core::psbt_unified::UnifiedPsbt::from_psbt(signed).unwrap(),
        &secp,
    )
    .unwrap()
}

#[test]
fn fork_transport_binds_chain_descriptor_and_exact_witness_then_sends_once() {
    let verified = verified_fork(ChainId::Bitcoin, &[0, 1]);
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let fresh_gate = || {
        SubmissionGate::for_claim_fork(
            &verified,
            std::time::Instant::now() + Duration::from_secs(60),
        )
        .0
    };
    for chain in [
        ChainId::Bitcoin,
        ChainId::Testnet4,
        ChainId::BitcoinBlake2bTestnet4,
    ] {
        assert_eq!(
            control(chain, verified.descriptor().clone(), backend.clone())
                .submit_verified_claim_fork(&verified, &fresh_gate()),
            Err(SubmissionError::UnsupportedChain),
        );
    }
    let other = CoincubeDescriptor::from_str(
        &verified
            .descriptor()
            .to_string()
            .split('#')
            .next()
            .unwrap()
            .replace("older(46)", "older(47)"),
    )
    .unwrap();
    assert_eq!(
        control(ChainId::BitcoinBlake2b, other, backend.clone())
            .submit_verified_claim_fork(&verified, &fresh_gate()),
        Err(SubmissionError::DescriptorMismatch),
    );
    let daemon = control(
        ChainId::BitcoinBlake2b,
        verified.descriptor().clone(),
        backend.clone(),
    );
    let other_witness = verified_fork(ChainId::Bitcoin, &[1, 2]);
    assert_eq!(
        verified.transaction().compute_txid(),
        other_witness.transaction().compute_txid()
    );
    assert_ne!(
        verified.transaction().compute_wtxid(),
        other_witness.transaction().compute_wtxid()
    );
    let gate = fresh_gate();
    assert_eq!(
        daemon.submit_verified_claim_fork(&other_witness, &gate),
        Err(SubmissionError::GateMismatch)
    );
    assert_eq!(gate.state(), SubmissionState::Pending);
    assert!(backend
        .lock()
        .unwrap()
        .broadcasted
        .lock()
        .unwrap()
        .is_empty());
    assert_eq!(
        daemon.submit_verified_claim_fork(&verified, &gate),
        Ok(SubmissionOutcome::UpstreamAccepted {
            txid: verified.transaction().compute_txid(),
            wtxid: verified.transaction().compute_wtxid(),
        })
    );
    assert_eq!(
        daemon.submit_verified_claim_fork(&verified, &gate),
        Err(SubmissionError::AlreadyStarted)
    );
    let backend = backend.lock().unwrap();
    let sent = backend.broadcasted.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        bitcoin::consensus::serialize(&sent[0]),
        bitcoin::consensus::serialize(verified.transaction())
    );
}

#[test]
fn fork_transport_revocation_expiry_testnet_refusal_and_uncertain_response() {
    let verified = verified_fork(ChainId::Bitcoin, &[0, 1]);
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let daemon = control(
        ChainId::BitcoinBlake2b,
        verified.descriptor().clone(),
        backend.clone(),
    );
    let (gate, revoker) = SubmissionGate::for_claim_fork(
        &verified,
        std::time::Instant::now() + Duration::from_secs(60),
    );
    assert_eq!(revoker.revoke(), SubmissionState::Revoked);
    assert_eq!(
        daemon.submit_verified_claim_fork(&verified, &gate),
        Err(SubmissionError::Revoked)
    );
    let (gate, _) = SubmissionGate::for_claim_fork(&verified, std::time::Instant::now());
    assert_eq!(
        daemon.submit_verified_claim_fork(&verified, &gate),
        Err(SubmissionError::Expired)
    );
    let testnet = verified_fork(ChainId::Testnet4, &[0, 1]);
    let (gate, _) = SubmissionGate::for_claim_fork(
        &testnet,
        std::time::Instant::now() + Duration::from_secs(60),
    );
    assert_eq!(
        control(
            ChainId::BitcoinBlake2bTestnet4,
            testnet.descriptor().clone(),
            backend.clone()
        )
        .submit_verified_claim_fork(&testnet, &gate),
        Err(SubmissionError::UnsupportedChain),
    );
    assert!(backend
        .lock()
        .unwrap()
        .broadcasted
        .lock()
        .unwrap()
        .is_empty());
    backend.lock().unwrap().broadcast_error =
        Some("response lost after possible submission".into());
    let (gate, _) = SubmissionGate::for_claim_fork(
        &verified,
        std::time::Instant::now() + Duration::from_secs(60),
    );
    assert_eq!(
        daemon.submit_verified_claim_fork(&verified, &gate),
        Err(SubmissionError::Uncertain {
            txid: verified.transaction().compute_txid(),
            wtxid: verified.transaction().compute_wtxid(),
        })
    );
    assert_eq!(gate.state(), SubmissionState::Started);
    assert_eq!(
        daemon.submit_verified_claim_fork(&verified, &gate),
        Err(SubmissionError::AlreadyStarted)
    );
    assert_eq!(backend.lock().unwrap().broadcasted.lock().unwrap().len(), 1);
}

#[cfg(feature = "regtest-harness")]
mod regtest_transport_tests {
    use super::super::regtest_harness::RegtestTransport;
    use super::*;
    use std::{
        io::{Read, Write},
        net::{SocketAddr, TcpListener},
        sync::atomic::AtomicBool,
    };
    struct Server {
        addr: SocketAddr,
        requests: Arc<Mutex<Vec<serde_json::Value>>>,
        mainnet: Arc<AtomicBool>,
        oversized: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Server {
        fn new(txid: Txid, redirect: bool) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            listener.set_nonblocking(true).unwrap();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let mainnet = Arc::new(AtomicBool::new(false));
            let oversized = Arc::new(AtomicBool::new(false));
            let huge = oversized.clone();
            let stop = Arc::new(AtomicBool::new(false));
            let (log, changed, done) = (requests.clone(), mainnet.clone(), stop.clone());
            let thread = std::thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(pair) => pair,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(2));
                            continue;
                        }
                        Err(error) => panic!("{}", error),
                    };
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .unwrap();
                    let mut headers = Vec::new();
                    while !headers.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        stream.read_exact(&mut byte).unwrap();
                        headers.push(byte[0]);
                        assert!(headers.len() < 8192);
                    }
                    let headers = String::from_utf8(headers).unwrap();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse().unwrap())
                        })
                        .unwrap();
                    let mut body = vec![0; length];
                    stream.read_exact(&mut body).unwrap();
                    let request: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    log.lock().unwrap().push(request.clone());
                    if redirect {
                        write!(stream, "HTTP/1.1 302 Found\r\nLocation: http://{}/elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", addr).unwrap();
                        continue;
                    }
                    let result = if huge.load(Ordering::SeqCst) {
                        serde_json::json!({"chain":"regtest", "padding":"x".repeat(1024 * 1024)})
                    } else {
                        match request["method"].as_str().unwrap() {
                            "getblockchaininfo" => {
                                serde_json::json!({"chain": if changed.load(Ordering::SeqCst) { "main" } else { "regtest" }})
                            }
                            "sendrawtransaction" => serde_json::json!(txid),
                            method => panic!("unexpected method {}", method),
                        }
                    };
                    let body =
                        serde_json::json!({"jsonrpc":"2.0", "id":request["id"], "result":result})
                            .to_string();
                    // Oversize refusal may close the socket while this fixture
                    // is still sending its deliberately excessive body.
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                }
            });
            Self {
                addr,
                requests,
                mainnet,
                oversized,
                stop,
                thread: Some(thread),
            }
        }
        fn sends(&self) -> Vec<serde_json::Value> {
            self.requests
                .lock()
                .unwrap()
                .iter()
                .filter(|r| r["method"] == "sendrawtransaction")
                .cloned()
                .collect()
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            self.thread.take().unwrap().join().unwrap();
        }
    }
    #[test]
    fn local_regtest_transport_uses_exact_bytes_and_one_use_gate() {
        let (built, signers) = fixture(ChainId::Bitcoin, false);
        let verified = finalize_poison_transfer(
            &built,
            &sign(&built, &signers[..2]),
            &secp256k1::Secp256k1::verification_only(),
        )
        .unwrap();
        let server = Server::new(verified.transaction().compute_txid(), false);
        let transport = RegtestTransport::new(
            server.addr,
            "synthetic:only",
            ChainId::Bitcoin,
            built.descriptor().clone(),
        )
        .unwrap();
        let (gate, _) = SubmissionGate::new(
            &verified,
            std::time::Instant::now() + Duration::from_secs(10),
        );
        assert!(transport.submit_poison(&verified, &gate).is_ok());
        assert_eq!(
            transport.submit_poison(&verified, &gate),
            Err(SubmissionError::AlreadyStarted)
        );
        let sends = server.sends();
        assert_eq!(sends.len(), 1);
        assert_eq!(
            sends[0]["params"][0],
            miniscript::bitcoin::consensus::encode::serialize_hex(verified.transaction())
        );
        for expired in [false, true] {
            let deadline = std::time::Instant::now()
                + if expired {
                    Duration::ZERO
                } else {
                    Duration::from_secs(10)
                };
            let (gate, revoker) = SubmissionGate::new(&verified, deadline);
            if !expired {
                revoker.revoke();
            }
            assert_eq!(
                transport.submit_poison(&verified, &gate),
                Err(if expired {
                    SubmissionError::Expired
                } else {
                    SubmissionError::Revoked
                })
            );
        }
        assert_eq!(server.sends().len(), 1);
        server.mainnet.store(true, Ordering::SeqCst);
        let (gate, _) = SubmissionGate::new(
            &verified,
            std::time::Instant::now() + Duration::from_secs(10),
        );
        assert_eq!(
            transport.submit_poison(&verified, &gate),
            Err(SubmissionError::UnsupportedChain)
        );
        assert_eq!(gate.state(), SubmissionState::Pending);
        assert_eq!(server.sends().len(), 1);
    }
    #[test]
    fn regtest_transport_refuses_nonlocal_nonregtest_and_redirects() {
        let (built, _) = fixture(ChainId::Bitcoin, false);
        assert!(matches!(
            RegtestTransport::new(
                "192.0.2.1:8332".parse().unwrap(),
                "synthetic:only",
                ChainId::Bitcoin,
                built.descriptor().clone()
            ),
            Err(SubmissionError::UnsupportedChain)
        ));
        let server = Server::new(Txid::from_byte_array([1; 32]), false);
        server.mainnet.store(true, Ordering::SeqCst);
        assert!(matches!(
            RegtestTransport::new(
                server.addr,
                "synthetic:only",
                ChainId::Bitcoin,
                built.descriptor().clone()
            ),
            Err(SubmissionError::UnsupportedChain)
        ));
        let redirect = Server::new(Txid::from_byte_array([1; 32]), true);
        assert!(RegtestTransport::new(
            redirect.addr,
            "synthetic:only",
            ChainId::Bitcoin,
            built.descriptor().clone()
        )
        .is_err());
        assert_eq!(redirect.requests.lock().unwrap().len(), 1);
        let oversized = Server::new(Txid::from_byte_array([1; 32]), false);
        oversized.oversized.store(true, Ordering::SeqCst);
        assert!(RegtestTransport::new(
            oversized.addr,
            "synthetic:only",
            ChainId::Bitcoin,
            built.descriptor().clone()
        )
        .is_err());
    }
}

#[test]
fn ancestry_transport_binds_exact_witness_and_preserves_one_use_refusals() {
    use coincube_core::{
        claim_ancestry::{verify, Link},
        claim_finalize::finalize_ancestry_transfer,
        claim_spend::create_ancestry_self_transfer,
    };
    for recovery in [false, true] {
        let (descriptor, mut getter, coins, signers) = fixture_inputs(recovery);
        let raw = bitcoin::consensus::serialize(&getter.0[&coins[0].outpoint.txid]);
        let dependency = verify(
            coins[0].outpoint,
            &[Link {
                transaction: &raw,
                parent_input: None,
            }],
        )
        .unwrap();
        let secp = secp256k1::Secp256k1::new();
        let built = create_ancestry_self_transfer(
            ChainId::Bitcoin,
            &descriptor,
            &secp256k1::Secp256k1::verification_only(),
            &mut getter,
            &coins,
            ChildNumber::from_normal_idx(12).unwrap(),
            5,
            absolute::LockTime::ZERO,
            &dependency,
        )
        .unwrap();
        let selected = if recovery {
            &signers[3..]
        } else {
            &signers[..2]
        };
        let signed = selected.iter().fold(built.psbt().clone(), |psbt, signer| {
            signer.sign_psbt(psbt, &secp).unwrap()
        });
        let verified = finalize_ancestry_transfer(&built, &signed, &secp).unwrap();
        let deadline = || std::time::Instant::now() + Duration::from_secs(60);
        let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
        let daemon = control(ChainId::Bitcoin, descriptor.clone(), backend.clone());
        let (gate, revoker) = SubmissionGate::for_ancestry(&verified, deadline());
        revoker.revoke();
        assert_eq!(
            daemon.submit_verified_ancestry(&verified, &gate),
            Err(SubmissionError::Revoked)
        );
        let (expired, _) = SubmissionGate::for_ancestry(&verified, std::time::Instant::now());
        assert_eq!(
            daemon.submit_verified_ancestry(&verified, &expired),
            Err(SubmissionError::Expired)
        );
        let (gate, _) = SubmissionGate::for_ancestry(&verified, deadline());
        for chain in [ChainId::BitcoinBlake2b, ChainId::Testnet4] {
            assert_eq!(
                control(chain, descriptor.clone(), backend.clone())
                    .submit_verified_ancestry(&verified, &gate),
                Err(SubmissionError::UnsupportedChain)
            );
        }
        let other = CoincubeDescriptor::from_str(
            &descriptor
                .to_string()
                .split('#')
                .next()
                .unwrap()
                .replace("older(46)", "older(47)"),
        )
        .unwrap();
        assert_eq!(
            control(ChainId::Bitcoin, other, backend.clone())
                .submit_verified_ancestry(&verified, &gate),
            Err(SubmissionError::DescriptorMismatch)
        );
        if !recovery {
            let alternate_signed = signers[1..3]
                .iter()
                .fold(built.psbt().clone(), |psbt, signer| {
                    signer.sign_psbt(psbt, &secp).unwrap()
                });
            let alternate = finalize_ancestry_transfer(&built, &alternate_signed, &secp).unwrap();
            assert_eq!(
                alternate.transaction().compute_txid(),
                verified.transaction().compute_txid()
            );
            assert_ne!(
                alternate.transaction().compute_wtxid(),
                verified.transaction().compute_wtxid()
            );
            assert_eq!(
                daemon.submit_verified_ancestry(&alternate, &gate),
                Err(SubmissionError::GateMismatch)
            );
        }
        assert_eq!(gate.state(), SubmissionState::Pending);
        assert!(backend
            .lock()
            .unwrap()
            .broadcasted
            .lock()
            .unwrap()
            .is_empty());
        let txid = verified.transaction().compute_txid();
        let wtxid = verified.transaction().compute_wtxid();
        assert_eq!(
            daemon.submit_verified_ancestry(&verified, &gate),
            Ok(SubmissionOutcome::UpstreamAccepted { txid, wtxid })
        );
        assert_eq!(
            daemon.submit_verified_ancestry(&verified, &gate),
            Err(SubmissionError::AlreadyStarted)
        );
        assert_eq!(
            *backend.lock().unwrap().broadcasted.lock().unwrap(),
            vec![verified.transaction().clone()]
        );
        backend.lock().unwrap().broadcast_error = Some("connection lost".into());
        let (gate, _) = SubmissionGate::for_ancestry(&verified, deadline());
        assert_eq!(
            daemon.submit_verified_ancestry(&verified, &gate),
            Err(SubmissionError::Uncertain { txid, wtxid })
        );
        assert_eq!(
            daemon.submit_verified_ancestry(&verified, &gate),
            Err(SubmissionError::AlreadyStarted)
        );
        assert_eq!(backend.lock().unwrap().broadcasted.lock().unwrap().len(), 2);
    }
}
