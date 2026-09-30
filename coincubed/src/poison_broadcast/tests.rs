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

/// The daemon's Claim fork route, from the signing PSBT to the transport: the
/// coordinator finalises (`Preparation::finish`), then submits the artifact.
fn submit_fork_psbt(
    daemon: &DaemonControl,
    sweep: &coincube_core::claim_spend::ClaimForkSweep,
    signed: &coincube_core::psbt_unified::UnifiedPsbt,
) -> Result<SubmissionOutcome, String> {
    let secp = secp256k1::Secp256k1::verification_only();
    let verified = coincube_core::claim_finalize::finalize_claim_fork_sweep(sweep, signed, &secp)
        .map_err(|e| format!("{:?}", e))?;
    let (gate, _) = SubmissionGate::for_claim_fork(
        &verified,
        std::time::Instant::now() + Duration::from_secs(60),
    );
    daemon
        .submit_verified_claim_fork(&verified, &gate)
        .map_err(|e| format!("{:?}", e))
}

#[test]
fn fork_transport_never_receives_a_psbt_retaining_a_legacy_alternative() {
    use coincube_core::{
        psbt_unified::{merge_signatures, UnifiedPsbt},
        unified_signing::sign_p2wsh_all_unified,
    };
    let (sweep, signers) = fork_fixture(ChainId::Bitcoin);
    let secp = secp256k1::Secp256k1::new();
    let unified = sign_p2wsh_all_unified(
        &signers[0],
        &UnifiedPsbt::from_psbt(sweep.psbt().clone()).unwrap(),
        &secp,
    )
    .unwrap();
    let with_legacy = |indices: &[usize]| {
        let legacy = indices.iter().fold(sweep.psbt().clone(), |psbt, i| {
            signers[*i].sign_psbt(psbt, &secp).unwrap()
        });
        let mut mixed = unified.clone();
        merge_signatures(&mut mixed, &UnifiedPsbt::from_psbt(legacy).unwrap()).unwrap();
        mixed
    };
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let daemon = control(
        ChainId::BitcoinBlake2b,
        sweep.descriptor().clone(),
        backend.clone(),
    );
    // The retained legacy signatures of keys 1 and 2 meet multi(2) without the
    // unified signature: refused before any artifact reaches the transport.
    let refused = submit_fork_psbt(&daemon, &sweep, &with_legacy(&[1, 2]));
    assert!(
        refused
            .as_ref()
            .is_err_and(|e| e.contains("UnsafeLegacyAlternative")),
        "{:?}",
        refused
    );
    assert!(backend
        .lock()
        .unwrap()
        .broadcasted
        .lock()
        .unwrap()
        .is_empty());
    // Control: the unified signature plus the one legacy signature it needs.
    let sent = submit_fork_psbt(&daemon, &sweep, &with_legacy(&[1])).unwrap();
    let SubmissionOutcome::UpstreamAccepted { txid, .. } = sent;
    assert_eq!(txid, sweep.psbt().unsigned_tx.compute_txid());
    assert_eq!(backend.lock().unwrap().broadcasted.lock().unwrap().len(), 1);
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

#[test]
fn claim_backend_binding_detects_same_address_replacement_and_private_config_changes() {
    use crate::config::{BitcoinBackend, BitcoindConfig, BitcoindRpcAuth};
    let (descriptor, _, _, _) = fixture_inputs(false);
    let mut daemon = control(
        ChainId::Bitcoin,
        descriptor,
        Arc::new(Mutex::new(DummyBitcoind::new())),
    );
    let node = BitcoindConfig {
        addr: "127.0.0.1:18443".parse().unwrap(),
        rpc_auth: BitcoindRpcAuth::UserPass("synthetic-user".into(), "synthetic-secret".into()),
    };
    daemon.config.bitcoin_backend = Some(BitcoinBackend::Bitcoind(node.clone()));
    let binding = daemon.claim_backend_binding();
    assert_eq!(binding, daemon.clone().claim_backend_binding());
    assert!(daemon.check_claim_backend_binding(&binding).is_ok());
    let mut replacement = daemon.clone();
    replacement.bitcoin = Arc::new(Mutex::new(DummyBitcoind::new()));
    assert_eq!(
        replacement.config.bitcoin_backend,
        daemon.config.bitcoin_backend
    );
    assert_ne!(binding, replacement.claim_backend_binding());
    assert_eq!(
        replacement.check_claim_backend_binding(&binding),
        Err(SubmissionError::BackendUnavailable)
    );
    let mut changed = daemon.clone();
    let mut changed_node = node;
    changed_node.rpc_auth =
        BitcoindRpcAuth::UserPass("synthetic-user".into(), "different-secret".into());
    changed.config.bitcoin_backend = Some(BitcoinBackend::Bitcoind(changed_node));
    assert_eq!(
        changed.check_claim_backend_binding(&binding),
        Err(SubmissionError::BackendUnavailable)
    );
    changed.config = daemon.config.clone();
    changed.config.bitcoin_config.chain = ChainId::BitcoinBlake2b;
    assert_eq!(
        changed.check_claim_backend_binding(&binding),
        Err(SubmissionError::BackendUnavailable)
    );
    let debug = format!("{:?}", binding);
    for private in ["synthetic-user", "synthetic-secret", "127.0.0.1"] {
        assert!(!debug.contains(private));
    }
    drop(changed);
    drop(replacement);
    let weak = Arc::downgrade(&daemon.bitcoin);
    drop(daemon);
    assert!(
        weak.upgrade().is_none(),
        "a review must not keep the backend alive"
    );
}

#[test]
fn bound_transports_send_exact_bytes_once_and_never_follow_redirects() {
    use crate::config::{BitcoinBackend, BitcoindConfig, BitcoindRpcAuth};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    let (built, signers) = fixture(ChainId::Bitcoin, false);
    let verified = finalize_poison_transfer(
        &built,
        &sign(&built, &signers[..2]),
        &secp256k1::Secp256k1::verification_only(),
    )
    .unwrap();
    for (connect, case) in [false, true]
        .iter()
        .copied()
        .flat_map(|connect| (0..8).map(move |case| (connect, case)))
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let tx = verified.transaction().clone();
        let worker = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    other => panic!("node fixture accept failed: {:?}", other),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(2)))
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
            assert!(length < 16_384);
            let mut bytes = vec![0; length];
            stream.read_exact(&mut bytes).unwrap();
            if connect {
                assert!(headers.starts_with("POST /api/v1/esplora/bitcoin/mainnet/tx HTTP/1.1\r\n"));
                assert!(!headers.to_ascii_lowercase().contains("authorization:"));
                assert_eq!(
                    bytes,
                    bitcoin::consensus::encode::serialize_hex(&tx).as_bytes()
                );
            } else {
                let request: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(
                    request,
                    serde_json::json!({"jsonrpc":"2.0","id":1,"method":"sendrawtransaction","params":[bitcoin::consensus::encode::serialize_hex(&tx)]})
                );
            }
            let status = match case {
                1 => "401 Unauthorized",
                2 => "307 Temporary Redirect",
                _ => "200 OK",
            };
            let body = if connect {
                match case {
                    3 => Txid::all_zeros().to_string(),
                    4 => format!("{{\"result\":\"{}\"}}", tx.compute_txid()),
                    5 => "not a transaction id".into(),
                    6 => "x".repeat(16_385),
                    _ => tx.compute_txid().to_string(),
                }
            } else {
                match case {
                    3 => serde_json::json!({"id":1,"result":Txid::all_zeros(),"error":null})
                        .to_string(),
                    4 => serde_json::json!({"id":2,"result":tx.compute_txid(),"error":null})
                        .to_string(),
                    5 => "not JSON".into(),
                    6 => "x".repeat(16_385),
                    _ => serde_json::json!({"id":1,"result":tx.compute_txid(),"error":null})
                        .to_string(),
                }
            };
            if case == 7 {
                // The node has received every transaction byte but sends no
                // acknowledgement before the client's total timeout.
                std::thread::sleep(Duration::from_secs(16));
            } else {
                let written = write!(stream, "HTTP/1.1 {}\r\nContent-Length: {}\r\nLocation: http://{}/redirected\r\nConnection: close\r\n\r\n{}", status, body.len(), address, body);
                // An oversized Content-Length may make the client close before
                // the fixture finishes writing the deliberately rejected body.
                if case != 6 {
                    written.unwrap();
                }
            }
            drop(stream);
            let deadline = std::time::Instant::now() + Duration::from_millis(200);
            while std::time::Instant::now() < deadline {
                assert!(
                    matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock),
                    "sender retried or followed a redirect"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        let mut daemon = control(
            ChainId::Bitcoin,
            built.descriptor().clone(),
            Arc::new(Mutex::new(DummyBitcoind::new())),
        );
        daemon.config.bitcoin_backend = Some(BitcoinBackend::Bitcoind(BitcoindConfig {
            addr: address,
            rpc_auth: BitcoindRpcAuth::UserPass("synthetic".into(), "fixture".into()),
        }));
        let binding = daemon.claim_backend_binding();
        let (gate, revoker) = SubmissionGate::new(
            &verified,
            std::time::Instant::now() + Duration::from_secs(10),
        );
        let origin = format!("http://{}/", address);
        let submit = || {
            if connect {
                daemon.submit_verified_poison_to_connect(&verified, &binding, &gate, &origin)
            } else {
                daemon.submit_verified_poison_to_node(&verified, &binding, &gate)
            }
        };
        let result = submit();
        if case == 0 {
            assert!(matches!(
                result,
                Ok(SubmissionOutcome::UpstreamAccepted { .. })
            ));
        } else {
            assert!(matches!(result, Err(SubmissionError::Uncertain { .. })));
        }
        assert_eq!(revoker.state(), SubmissionState::Started);
        assert_eq!(submit(), Err(SubmissionError::AlreadyStarted));
        worker.join().unwrap();
    }
}

#[test]
fn direct_node_binding_and_revocation_prevent_any_http_attempt() {
    use crate::config::{BitcoinBackend, BitcoindConfig, BitcoindRpcAuth};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let (built, signers) = fixture(ChainId::Bitcoin, false);
    let verified = finalize_poison_transfer(
        &built,
        &sign(&built, &signers[..2]),
        &secp256k1::Secp256k1::verification_only(),
    )
    .unwrap();
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let mut daemon = control(
        ChainId::Bitcoin,
        built.descriptor().clone(),
        backend.clone(),
    );
    daemon.config.bitcoin_backend = Some(BitcoinBackend::Bitcoind(BitcoindConfig {
        addr: listener.local_addr().unwrap(),
        rpc_auth: BitcoindRpcAuth::UserPass("synthetic".into(), "fixture".into()),
    }));
    let binding = daemon.claim_backend_binding();
    let mut invalid_auth = daemon.clone();
    // A directory cannot contain cookie credentials. Preparation must fail
    // before consuming a gate and must never attempt HTTP authentication.
    invalid_auth.config.bitcoin_backend = Some(BitcoinBackend::Bitcoind(BitcoindConfig {
        addr: listener.local_addr().unwrap(),
        rpc_auth: BitcoindRpcAuth::CookieFile(std::env::temp_dir()),
    }));
    let (auth_gate, _auth_revoker) = SubmissionGate::new(
        &verified,
        std::time::Instant::now() + Duration::from_secs(30),
    );
    assert_eq!(
        invalid_auth.submit_verified_poison_to_node(
            &verified,
            &invalid_auth.claim_backend_binding(),
            &auth_gate,
        ),
        Err(SubmissionError::BackendUnavailable)
    );
    assert_eq!(auth_gate.state(), SubmissionState::Pending);
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
    let mut replacement = daemon.clone();
    replacement.bitcoin = Arc::new(Mutex::new(DummyBitcoind::new()));
    let (mut gate, revoker) = SubmissionGate::new(
        &verified,
        std::time::Instant::now() + Duration::from_secs(30),
    );
    assert_eq!(
        replacement.submit_verified_poison_to_node(&verified, &binding, &gate),
        Err(SubmissionError::BackendUnavailable)
    );
    assert_eq!(gate.state(), SubmissionState::Pending);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    gate.before_lock = Some(barrier.clone());
    let locked = backend.lock().unwrap();
    let worker = std::thread::spawn(move || {
        daemon.submit_verified_poison_to_node(&verified, &binding, &gate)
    });
    barrier.wait();
    assert_eq!(revoker.revoke(), SubmissionState::Revoked);
    drop(locked);
    assert_eq!(worker.join().unwrap(), Err(SubmissionError::Revoked));
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}

#[test]
fn connect_binding_origin_and_revocation_refuse_before_http() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let (built, signers) = fixture(ChainId::Bitcoin, false);
    let verified = finalize_poison_transfer(
        &built,
        &sign(&built, &signers[..2]),
        &secp256k1::Secp256k1::verification_only(),
    )
    .unwrap();
    let backend = Arc::new(Mutex::new(DummyBitcoind::new()));
    let daemon = control(
        ChainId::Bitcoin,
        built.descriptor().clone(),
        backend.clone(),
    );
    let binding = daemon.claim_backend_binding();
    let (mut gate, revoker) = SubmissionGate::new(
        &verified,
        std::time::Instant::now() + Duration::from_secs(30),
    );
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
            daemon.submit_verified_poison_to_connect(&verified, &binding, &gate, &invalid),
            Err(SubmissionError::BackendUnavailable)
        );
        assert_eq!(gate.state(), SubmissionState::Pending);
    }
    let mut replacement = daemon.clone();
    replacement.bitcoin = Arc::new(Mutex::new(DummyBitcoind::new()));
    assert_eq!(
        replacement.submit_verified_poison_to_connect(&verified, &binding, &gate, &origin),
        Err(SubmissionError::BackendUnavailable)
    );
    assert_eq!(gate.state(), SubmissionState::Pending);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    gate.before_lock = Some(barrier.clone());
    let locked = backend.lock().unwrap();
    let worker = std::thread::spawn(move || {
        daemon.submit_verified_poison_to_connect(&verified, &binding, &gate, &origin)
    });
    barrier.wait();
    assert_eq!(revoker.revoke(), SubmissionState::Revoked);
    drop(locked);
    assert_eq!(worker.join().unwrap(), Err(SubmissionError::Revoked));
    assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}
