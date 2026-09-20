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
fn fixture(chain: ChainId, recovery: bool) -> (PoisonSelfTransfer, Vec<MasterSigner>) {
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
