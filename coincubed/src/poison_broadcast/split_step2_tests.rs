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
    claim::BlockRef,
    foreign_split::{
        create_split_step1, create_split_step2, finalize_split_step2, SplitBranch, SplitCoin,
        SplitInputs, SplitSource, SplitStep2Inputs,
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
        daemon.submit_verified_split_step2(&verified, index(TARGET_INDEX), &gate),
        Ok(SubmissionOutcome::UpstreamAccepted {
            txid: verified.transaction().compute_txid(),
            wtxid: verified.transaction().compute_wtxid(),
        })
    );
    assert_eq!(
        daemon.submit_verified_split_step2(&verified, index(TARGET_INDEX), &gate),
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
        assert_eq!(
            control(chain, descriptor.clone(), backend.clone()).submit_verified_split_step2(
                &verified,
                index(TARGET_INDEX),
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
    assert_eq!(
        control(ChainId::BitcoinBlake2b, descriptor.clone(), backend.clone())
            .submit_verified_split_step2(&testnet, index(TARGET_INDEX), &gate),
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
            daemon.submit_verified_split_step2(&verified, wrong, &gate),
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
            daemon.submit_verified_split_step2(&elsewhere, index(TARGET_INDEX), &gate),
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
        daemon.submit_verified_split_step2(&verified, index(TARGET_INDEX), &gate),
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
        daemon.submit_verified_split_step2(&verified, index(TARGET_INDEX), &wrong_chain_gate),
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
        daemon.submit_verified_split_step2(&verified, index(TARGET_INDEX), &revoked),
        Err(SubmissionError::Revoked)
    );
    let (expired, _) = SubmissionGate::for_split_step2(&verified, Instant::now());
    assert_eq!(
        daemon.submit_verified_split_step2(&verified, index(TARGET_INDEX), &expired),
        Err(SubmissionError::Expired)
    );
    assert!(sent(&backend).is_empty());
    backend.lock().unwrap().broadcast_error = Some("response lost".into());
    let (gate, _) = fresh_gate(&verified);
    assert_eq!(
        daemon.submit_verified_split_step2(&verified, index(TARGET_INDEX), &gate),
        Err(SubmissionError::Uncertain {
            txid: verified.transaction().compute_txid(),
            wtxid: verified.transaction().compute_wtxid(),
        })
    );
    assert_eq!(gate.state(), SubmissionState::Started);
    assert_eq!(sent(&backend).len(), 1);
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
