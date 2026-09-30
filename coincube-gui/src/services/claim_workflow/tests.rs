use super::*;
use crate::services::claim_observation::{FailureKind, Stage};
use coincube_core::{
    claim::{
        BitcoinObservation, DeploymentObservation, DeploymentState, ForkObservation,
        ForkTransactionPresence, PreflightTips,
    },
    miniscript::bitcoin::{
        absolute, transaction, Amount, BlockHash, OutPoint, ScriptBuf, Sequence, TxIn, TxOut,
        Witness,
    },
};
use std::{fs, path::PathBuf};
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "claim-journal-{}-{}",
            std::process::id(),
            controller_id().unwrap()
        ));
        prepare_directory(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn context() -> Context {
    Context {
        generation: 1,
        account: "synthetic-account".into(),
        provider: "https://synthetic.invalid".into(),
    }
}
fn identity() -> WalletIdentity {
    WalletIdentity {
        bitcoin_cube: "btc-fixture".into(),
        fork_cube: "fork-fixture".into(),
        descriptor_digest: sha256::Hash::hash(b"synthetic-descriptor"),
    }
}
/// Reopen a journal whose previous owner was just dropped (#586).
///
/// While another test in this binary spawns a process, the child can briefly
/// hold duplicates of this process's descriptors until it execs, including
/// the dropped owner's `claim.lock`. `flock` belongs to the open file, not the
/// descriptor, so the first reopen can see `Busy` for a moment. Only `Busy` is
/// retried, every 10 ms for at most 5 s. Every other result, and a `Busy`
/// that outlasts the deadline, is returned unchanged.
fn reopen_settled(
    directory: &std::path::Path,
    identity: &WalletIdentity,
    context: Context,
) -> Result<Controller, Error> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match Controller::reopen(directory, identity, context.clone()) {
            Err(Error::Busy) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            result => return result,
        }
    }
}
/// Names a reopen result in assertion messages; `Controller` is not `Debug`.
fn outcome(result: &Result<Controller, Error>) -> String {
    match result {
        Ok(_) => "Ok(Controller)".into(),
        Err(error) => format!("Err({:?})", error),
    }
}
fn hash(n: u8) -> BlockHash {
    BlockHash::from_byte_array([n; 32])
}
fn policy() -> Policy {
    Policy {
        max_observation_age_seconds: 60,
        expiry_margin_seconds: 600,
    }
}
fn plan() -> ClaimPlan {
    let prev = OutPoint {
        txid: Txid::from_byte_array([3; 32]),
        vout: 0,
    };
    let mut script = vec![0x6a, 0x4c, 87];
    script.extend([1; 87]);
    ClaimPlan {
        bitcoin_chain: ChainId::Bitcoin,
        fork_chain: ChainId::BitcoinBlake2b,
        step1: Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: prev,
                script_sig: ScriptBuf::new(),
                sequence: Sequence::MAX,
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::ZERO,
                script_pubkey: ScriptBuf::from_bytes(script),
            }],
        },
        claimed_prevouts: vec![prev],
        poison: Poison::OpReturn,
        previous_confirmation: None,
    }
}
fn controller(temp: &Temp) -> Controller {
    Controller::create_intent(&temp.0, identity(), plan(), context(), None).unwrap()
}
fn observation(confirmed: bool) -> CollectedAssessment {
    let tip = BlockRef {
        height: 105,
        hash: hash(1),
    };
    let fork = BlockRef {
        height: 100,
        hash: hash(2),
    };
    CollectedAssessment {
        generation: 1,
        assessment: Assessment::ObservationsEligibleForPreflight,
        observations: ObservationBundle {
            bitcoin_transaction: if confirmed {
                crate::services::claim_observation::TransactionObservation::Confirmed {
                    txid: plan().step1.compute_txid(),
                    block: BlockRef {
                        height: 100,
                        hash: hash(4),
                    },
                }
            } else {
                crate::services::claim_observation::TransactionObservation::Absent
            },
            bitcoin: BitcoinObservation {
                chain: ChainId::Bitcoin,
                tip,
                location: if confirmed {
                    TransactionLocation::Confirmed {
                        txid: plan().step1.compute_txid(),
                        block: BlockRef {
                            height: 100,
                            hash: hash(4),
                        },
                        best_chain_hash_at_height: hash(4),
                    }
                } else {
                    TransactionLocation::Unconfirmed
                },
                observed_at: 10000,
            },
            fork: ForkObservation {
                chain: ChainId::BitcoinBlake2b,
                step1_txid: plan().step1.compute_txid(),
                step1_presence: ForkTransactionPresence::NotObserved,
                tip: fork,
                median_time_past: 8000,
                observed_at: 10000,
            },
            deployment: DeploymentObservation {
                chain: ChainId::BitcoinBlake2b,
                tip: fork,
                state: DeploymentState::Flagday {
                    height: 90,
                    expiry_time: 20000,
                    active: true,
                },
                observed_at: 10000,
            },
            preflight: PreflightTips { bitcoin: tip, fork },
        },
    }
}
fn refresh(c: &mut Controller, o: CollectedAssessment, now: i64) -> Status {
    let t = c.begin_check(&context()).unwrap();
    c.apply_observation(t, &context(), Ok(o), policy(), now)
        .unwrap()
}
fn signed() -> Transaction {
    let mut tx = plan().step1;
    tx.input[0].witness.push([1, 2, 3]);
    tx
}

#[test]
fn restart_is_unchecked_and_never_restores_construction_authority() {
    let temp = Temp::new();
    let mut c = controller(&temp);
    assert_eq!(
        refresh(&mut c, observation(true), 10000),
        Status::Observation(Assessment::ObservationsEligibleForPreflight)
    );
    assert!(c.last_inclusion().is_some());
    drop(c);
    let mut c = reopen_settled(&temp.0, &identity(), context()).unwrap();
    assert_eq!(c.status(), Status::Unchecked);
    assert!(c.last_inclusion().is_some());
    assert!(!c.construction_verified);
    assert!(matches!(
        c.record_broadcast_intent(&context(), &signed(), policy(), 10000),
        Err(Error::Unchecked)
    ));
}
#[test]
fn uncertain_broadcast_is_durable_and_reconciled_only_by_fresh_queries() {
    let temp = Temp::new();
    let mut c = controller(&temp);
    refresh(&mut c, observation(false), 10000);
    c.record_broadcast_intent(&context(), &signed(), policy(), 10000)
        .unwrap();
    assert_eq!(c.phase(), Phase::BroadcastUncertain);
    let id = c.signed_txid();
    drop(c);
    let mut c = reopen_settled(&temp.0, &identity(), context()).unwrap();
    assert_eq!(c.signed_txid(), id);
    refresh(&mut c, observation(false), 10000);
    assert_eq!(c.phase(), Phase::BroadcastUncertain);
    refresh(&mut c, observation(true), 10000);
    assert_eq!(c.phase(), Phase::Tracking);
    // A fresh loss of inclusion revokes the observation, but retains last known
    // inclusion so a subsequent check cannot silently forget the reorg.
    assert_eq!(
        refresh(&mut c, observation(false), 10000),
        Status::Observation(Assessment::Reorged)
    );
    assert!(c.fresh.is_none());
}
#[test]
fn stale_unavailable_and_tip_mismatch_replace_prior_eligibility() {
    let temp = Temp::new();
    let mut c = controller(&temp);
    refresh(&mut c, observation(true), 10000);
    assert_eq!(
        refresh(&mut c, observation(true), 10061),
        Status::Observation(Assessment::StaleObservation)
    );
    assert!(c.fresh.is_none());
    let ticket = c.begin_check(&context()).unwrap();
    assert_eq!(
        c.apply_observation(
            ticket,
            &context(),
            Err(Failure {
                stage: Stage::ForkAnchor,
                kind: FailureKind::Unavailable
            }),
            policy(),
            10000
        )
        .unwrap(),
        Status::Unavailable
    );
    let mut o = observation(false);
    o.observations.preflight.bitcoin.hash = hash(9);
    assert_eq!(
        refresh(&mut c, o, 10000),
        Status::Observation(Assessment::NeedsPreflightRecheck)
    );
}
#[test]
fn generation_provider_account_changes_and_late_results_are_rejected() {
    for field in 0..3 {
        let temp = Temp::new();
        let mut c = controller(&temp);
        let ticket = c.begin_check(&context()).unwrap();
        let mut changed = context();
        match field {
            0 => changed.generation += 1,
            1 => changed.provider.push_str("/other"),
            _ => changed.account.push_str("-other"),
        };
        assert!(matches!(
            c.apply_observation(ticket, &changed, Ok(observation(false)), policy(), 10000),
            Err(Error::Revoked)
        ));
        assert!(matches!(c.begin_check(&context()), Err(Error::Revoked)));
    }
    let temp = Temp::new();
    let mut c = controller(&temp);
    let old = c.begin_check(&context()).unwrap();
    let _new = c.begin_check(&context()).unwrap();
    assert!(matches!(
        c.apply_observation(old, &context(), Ok(observation(false)), policy(), 10000),
        Err(Error::LateObservation)
    ));
    assert_eq!(c.status(), Status::Unchecked);
}
#[test]
fn tickets_cannot_cross_controller_instances() {
    let a = Temp::new();
    let b = Temp::new();
    let mut first = controller(&a);
    let mut second = controller(&b);
    let ticket = first.begin_check(&context()).unwrap();
    let _ = second.begin_check(&context()).unwrap();
    assert!(matches!(
        second.apply_observation(ticket, &context(), Ok(observation(false)), policy(), 10000),
        Err(Error::LateObservation)
    ));
}
#[test]
fn replacement_and_noncooperative_journal_changes_are_refused() {
    let temp = Temp::new();
    let mut c = controller(&temp);
    refresh(&mut c, observation(false), 10000);
    let mut replacement = signed();
    replacement.output[0].value = Amount::from_sat(1);
    assert!(matches!(
        c.record_broadcast_intent(&context(), &replacement, policy(), 10000),
        Err(Error::InvalidPlan)
    ));
    fs::write(temp.0.join("intent.json"), b"{}").unwrap();
    let ticket = c.begin_check(&context()).unwrap();
    assert!(matches!(
        c.apply_observation(ticket, &context(), Ok(observation(false)), policy(), 10000),
        Err(Error::Conflict)
    ));
    assert_eq!(c.status(), Status::Unchecked);
}
#[test]
fn lock_conflicts_identity_mismatch_and_private_permissions_fail_closed() {
    let temp = Temp::new();
    let c = controller(&temp);
    assert!(matches!(
        Controller::reopen(&temp.0, &identity(), context()),
        Err(Error::Busy)
    ));
    drop(c);
    let mut wrong = identity();
    wrong.fork_cube.push('x');
    let reopened = reopen_settled(&temp.0, &wrong, context());
    assert!(
        matches!(reopened, Err(Error::WrongIdentity)),
        "{}",
        outcome(&reopened)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            temp.0.join("intent.json"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        let reopened = reopen_settled(&temp.0, &identity(), context());
        assert!(
            matches!(reopened, Err(Error::InvalidJournal)),
            "{}",
            outcome(&reopened)
        );
    }
}
#[cfg(unix)]
#[test]
fn failed_atomic_write_poisoned_owner_cannot_reuse_old_observations() {
    let temp = Temp::new();
    let mut c = controller(&temp);
    refresh(&mut c, observation(false), 10000);
    // Replace the destination directory with an absent path after taking ownership.
    // The owned lock survives, but writes fail; no broadcast receipt is returned.
    let moved = temp.0.with_extension("moved");
    fs::rename(&temp.0, &moved).unwrap();
    assert!(c
        .record_broadcast_intent(&context(), &signed(), policy(), 10000)
        .is_err());
    assert_eq!(c.status(), Status::Unchecked);
    fs::rename(moved, &temp.0).unwrap();
    let ticket = c.begin_check(&context()).unwrap();
    assert!(matches!(
        c.apply_observation(ticket, &context(), Ok(observation(false)), policy(), 10000),
        Err(Error::InvalidJournal)
    ));
    assert_eq!(c.status(), Status::Unchecked);
    assert!(c.fresh.is_none());
}

#[test]
fn restart_rejects_different_account_or_provider_but_not_new_generation() {
    let temp = Temp::new();
    drop(controller(&temp));
    let mut changed = context();
    changed.account.push('x');
    let reopened = reopen_settled(&temp.0, &identity(), changed);
    assert!(
        matches!(reopened, Err(Error::WrongIdentity)),
        "account change: {}",
        outcome(&reopened)
    );
    let mut changed = context();
    changed.provider.push('x');
    let reopened = reopen_settled(&temp.0, &identity(), changed);
    assert!(
        matches!(reopened, Err(Error::WrongIdentity)),
        "provider change: {}",
        outcome(&reopened)
    );
    let mut renewed = context();
    renewed.generation += 1;
    assert_eq!(
        reopen_settled(&temp.0, &identity(), renewed)
            .unwrap()
            .status(),
        Status::Unchecked
    );
}

#[cfg(unix)]
#[test]
fn reopen_settled_outwaits_only_a_transient_foreign_lock() {
    use fs4::fs_std::FileExt;
    use std::time::{Duration, Instant};
    // A second open file holding the flock stands in for a spawned child's
    // inherited duplicate of the dropped owner's descriptor (#586). It is held
    // until the test signals, then released 200 ms later.
    fn hold(temp: &Temp) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
        let foreign = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(temp.0.join("claim.lock"))
            .unwrap();
        // The acquire races a concurrent spawn exactly as a reopen does, so it
        // gets the same bounded wait.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !foreign.try_lock_exclusive().unwrap() {
            assert!(Instant::now() < deadline, "foreign lock never became free");
            std::thread::sleep(Duration::from_millis(10));
        }
        let (release, signal) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            signal.recv().unwrap();
            std::thread::sleep(Duration::from_millis(200));
            drop(foreign);
        });
        (release, holder)
    }
    let temp = Temp::new();
    drop(controller(&temp));
    // While the holder keeps the lock, a plain reopen is Busy: the CI failure.
    let (release, holder) = hold(&temp);
    let direct = Controller::reopen(&temp.0, &identity(), context());
    assert!(matches!(direct, Err(Error::Busy)), "{}", outcome(&direct));
    // Once the holder lets go, the real verdict comes through unchanged.
    release.send(()).unwrap();
    let mut changed = context();
    changed.provider.push('x');
    let reopened = reopen_settled(&temp.0, &identity(), changed);
    holder.join().unwrap();
    assert!(
        matches!(reopened, Err(Error::WrongIdentity)),
        "{}",
        outcome(&reopened)
    );
    let (release, holder) = hold(&temp);
    release.send(()).unwrap();
    let reopened = reopen_settled(&temp.0, &identity(), context());
    holder.join().unwrap();
    assert!(reopened.is_ok(), "{}", outcome(&reopened));
    // A holder that never lets go is still reported as Busy after the deadline.
    let started = Instant::now();
    let blocked = reopen_settled(&temp.0, &identity(), context());
    assert!(matches!(blocked, Err(Error::Busy)), "{}", outcome(&blocked));
    assert!(started.elapsed() >= Duration::from_secs(5));
    drop(reopened);
}

// Public synthetic descriptor fixture shared with core claim_spend tests.
const WSH_DESC: &str = "wsh(or_d(multi(1,[573fb35b/48'/1'/0'/2']tpubDFKp9T7WAYDcENSjoifkrpq1gMDF47KGJcJrpxzX23Qor8wuGbrEVs9utNq1MDS8E2WXJSBk1qoPQLpwyokW7DiUNPwFuxQkL7owNkLAb9W/<0;1>/*,[573fb35c/48'/1'/1'/2']tpubDFGezyzuHJPhdP3jHGW7v7Hwes4Hihqv5W2yyCmRY9VZJCRchETvxrMC8uECeJZdxQ14V4iD4DecoArkUSDwj8ogYE9WEv4MNZr12thNHCs/<0;1>/*),and_v(v:multi(2,[573fb35b/48'/1'/2'/2']tpubDDwxQauiaU964vPzt5Vd7jnDHEUtp2Vc34PaWpEXg5TQ3bRccxnc1MKKh88Hi7xiMeZo9Tm6fBcq4UGXqnDtGUniJLjqAD8SjQ8Eci3aSR7/<0;1>/*,[573fb35c/48'/1'/3'/2']tpubDE37XAVB5CQ1x85md3BQ5uHCoMwT5fgT8X13zzCUQ3x5o2jskYxKjj7Qcxt1Jpj4QB8tqspn2dooPCekRuQDYrDHov7J1ueUNu2wcvgRDxr/<0;1>/*),older(1000))))#fccaqlhh";

fn artifact(index: u32) -> coincube_core::claim_spend::PoisonSelfTransfer {
    use coincube_core::{
        descriptors::CoincubeDescriptor,
        miniscript::bitcoin::{bip32::ChildNumber, secp256k1},
        spend::{CandidateCoin, TxGetter},
    };
    use std::str::FromStr;
    let descriptor = CoincubeDescriptor::from_str(WSH_DESC).unwrap();
    let secp = secp256k1::Secp256k1::verification_only();
    let deriv = ChildNumber::from_normal_idx(0).unwrap();
    let tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn::default()],
        output: vec![TxOut {
            value: Amount::from_sat(100000),
            script_pubkey: descriptor
                .receive_descriptor()
                .derive(deriv, &secp)
                .script_pubkey(),
        }],
    };
    struct Getter(Transaction);
    impl TxGetter for Getter {
        fn get_tx(&mut self, id: &Txid) -> Option<Transaction> {
            (self.0.compute_txid() == *id).then(|| self.0.clone())
        }
    }
    let coin = CandidateCoin {
        outpoint: OutPoint::new(tx.compute_txid(), 0),
        amount: tx.output[0].value,
        deriv_index: deriv,
        is_change: false,
        must_select: true,
        sequence: None,
        ancestor_info: None,
    };
    coincube_core::claim_spend::create_poison_self_transfer(
        ChainId::Testnet4,
        &descriptor,
        &secp,
        &mut Getter(tx),
        &[coin],
        ChildNumber::from_normal_idx(index).unwrap(),
        5,
        absolute::LockTime::ZERO,
        hash(42),
    )
    .unwrap()
}
#[test]
fn public_admission_and_restart_revalidation_bind_the_opaque_artifact() {
    let temp = Temp::new();
    let built = artifact(10);
    let c = Controller::create(
        &temp.0,
        "bitcoin-cube".into(),
        "fork-cube".into(),
        &built,
        context(),
    )
    .unwrap();
    assert_eq!(c.plan().step1, built.psbt().unsigned_tx);
    assert_eq!(c.plan().fork_chain, ChainId::BitcoinBlake2bTestnet4);
    let identity = c.identity().clone();
    drop(c);
    let mut c = reopen_settled(&temp.0, &identity, context()).unwrap();
    assert!(!c.construction_verified);
    assert!(matches!(
        c.revalidate_construction(&context(), &artifact(11)),
        Err(Error::WrongIdentity)
    ));
    c.revalidate_construction(&context(), &built).unwrap();
    assert!(c.construction_verified);
    assert_eq!(c.status(), Status::Unchecked);
    assert_eq!(
        c.recorded_bitcoin_change_index(),
        Some(built.change_index())
    );
    assert_eq!(c.intent.version, 4);
    let mut bad = c.intent.clone();
    bad.bitcoin_change_index = None;
    assert!(validate(&bad).is_err());
    bad.bitcoin_change_index = Some(0x8000_0000);
    assert!(validate(&bad).is_err());
    // A well-formed but false hint never authenticates a construction.
    c.intent.bitcoin_change_index = Some(u32::from(built.change_index()) + 1);
    assert!(matches!(
        c.revalidate_construction(&context(), &built),
        Err(Error::WrongIdentity)
    ));
    assert!(!c.construction_verified);
    // Legacy records remain readable and gain the hint only by reconstruction.
    c.intent.version = 1;
    c.intent.bitcoin_change_index = None;
    c.journal.store(&c.intent).unwrap();
    drop(c);
    let mut c = reopen_settled(&temp.0, &identity, context()).unwrap();
    assert!(c.recorded_bitcoin_change_index().is_none());
    c.revalidate_construction(&context(), &built).unwrap();
    drop(c);
    let c = reopen_settled(&temp.0, &identity, context()).unwrap();
    assert_eq!(
        c.recorded_bitcoin_change_index(),
        Some(built.change_index())
    );
    assert!(!c.construction_verified);
}

#[test]
fn corrupted_unsigned_bytes_are_not_restored_as_valid_intent() {
    let temp = Temp::new();
    drop(controller(&temp));
    let path = temp.0.join("intent.json");
    let mut intent: Intent = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    intent.plan.step1.output[0].value = Amount::from_sat(1);
    fs::write(&path, serde_json::to_vec(&intent).unwrap()).unwrap();
    let reopened = reopen_settled(&temp.0, &identity(), context());
    assert!(
        matches!(reopened, Err(Error::InvalidPlan)),
        "{}",
        outcome(&reopened)
    );
}
#[cfg(unix)]
#[test]
fn journal_symlink_is_rejected_without_touching_target() {
    use std::os::unix::fs::symlink;
    let temp = Temp::new();
    let other = Temp::new();
    drop(controller(&temp));
    let path = temp.0.join("intent.json");
    let target = other.0.join("original");
    fs::rename(&path, &target).unwrap();
    let bytes = fs::read(&target).unwrap();
    symlink(&target, &path).unwrap();
    let reopened = reopen_settled(&temp.0, &identity(), context());
    assert!(
        reopened.is_err() && !matches!(reopened, Err(Error::Busy)),
        "{}",
        outcome(&reopened)
    );
    assert_eq!(fs::read(target).unwrap(), bytes);
}

use coincube_core::{
    bip39::Mnemonic,
    claim_finalize::{finalize_poison_transfer, VerifiedPoisonTransfer},
    claim_spend::{create_claim_fork_sweep, create_poison_self_transfer, PoisonSelfTransfer},
    descriptors::{CoincubeDescriptor, CoincubePolicy, PathInfo},
    miniscript::{
        bitcoin::{
            bip32::{ChildNumber, DerivationPath},
            secp256k1, Network,
        },
        DescriptorPublicKey,
    },
    signer::MasterSigner,
    spend::{CandidateCoin, TxGetter},
};
use std::str::FromStr;
fn real_artifact(
    chain: ChainId,
    multi: bool,
    change: u32,
) -> (PoisonSelfTransfer, VerifiedPoisonTransfer) {
    let secp = secp256k1::Secp256k1::new();
    let signers: Vec<_> = (40..44)
        .map(|b| {
            MasterSigner::from_mnemonic(Network::Bitcoin, Mnemonic::from_entropy(&[b; 16]).unwrap())
                .unwrap()
        })
        .collect();
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
    let primary = if multi {
        PathInfo::Multi(2, keys[..3].to_vec())
    } else {
        PathInfo::Single(keys[0].clone())
    };
    let desc = CoincubeDescriptor::new(
        CoincubePolicy::new_legacy(
            primary,
            std::iter::once((
                46,
                PathInfo::Single(keys[if multi { 3 } else { 2 }].clone()),
            ))
            .collect(),
        )
        .unwrap(),
    );
    let verify = secp256k1::Secp256k1::verification_only();
    let previous = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn::default()],
        output: vec![TxOut {
            value: Amount::from_sat(100000),
            script_pubkey: desc
                .receive_descriptor()
                .derive(0.into(), &verify)
                .script_pubkey(),
        }],
    };
    struct Getter(Transaction);
    impl TxGetter for Getter {
        fn get_tx(&mut self, id: &Txid) -> Option<Transaction> {
            (self.0.compute_txid() == *id).then(|| self.0.clone())
        }
    }
    let coin = CandidateCoin {
        outpoint: OutPoint::new(previous.compute_txid(), 0),
        amount: previous.output[0].value,
        deriv_index: 0.into(),
        is_change: false,
        must_select: true,
        sequence: None,
        ancestor_info: None,
    };
    let built = create_poison_self_transfer(
        chain,
        &desc,
        &verify,
        &mut Getter(previous),
        &[coin],
        ChildNumber::from_normal_idx(change).unwrap(),
        5,
        absolute::LockTime::ZERO,
        hash(42),
    )
    .unwrap();
    let signed = signers[0].sign_psbt(built.psbt().clone(), &secp).unwrap();
    let signed = if multi {
        signers[1].sign_psbt(signed, &secp).unwrap()
    } else {
        signed
    };
    let final_tx = finalize_poison_transfer(&built, &signed, &verify).unwrap();
    (built, final_tx)
}
fn sync_transaction_observation(o: &mut CollectedAssessment) {
    if let TransactionLocation::Confirmed { txid, block, .. } = o.observations.bitcoin.location {
        o.observations.bitcoin_transaction =
            crate::services::claim_observation::TransactionObservation::Confirmed { txid, block };
    }
}
fn real_observation(c: &Controller, depth: u64) -> CollectedAssessment {
    let mut result = observation(depth > 0);
    let id = c.plan().step1.compute_txid();
    result.observations.fork.step1_txid = id;
    if let TransactionLocation::Confirmed { txid, .. } = &mut result.observations.bitcoin.location {
        *txid = id;
    }
    result.observations.bitcoin.tip.height = 99 + depth.max(1);
    result.observations.preflight.bitcoin = result.observations.bitcoin.tip;
    sync_transaction_observation(&mut result);
    result
}

#[test]
fn fork_plan_requires_fresh_depth_and_survives_restart_without_authority() {
    let temp = Temp::new();
    let (source, verified) = real_artifact(ChainId::Bitcoin, true, 10);
    let mut c = Controller::create(
        &temp.0,
        "btc-fixture".into(),
        "fork-fixture".into(),
        &source,
        context(),
    )
    .unwrap();
    let wallet = c.identity().clone();
    let initial = real_observation(&c, 0);
    refresh(&mut c, initial, 10000);
    c.record_broadcast_intent(&context(), verified.transaction(), policy(), 10000)
        .unwrap();
    struct Getter(Transaction);
    impl TxGetter for Getter {
        fn get_tx(&mut self, id: &Txid) -> Option<Transaction> {
            (self.0.compute_txid() == *id).then(|| self.0.clone())
        }
    }
    let previous = source.psbt().inputs[0].non_witness_utxo.clone().unwrap();
    let coin = CandidateCoin {
        outpoint: source.psbt().unsigned_tx.input[0].previous_output,
        amount: previous.output[0].value,
        deriv_index: 0.into(),
        is_change: false,
        must_select: false,
        sequence: None,
        ancestor_info: None,
    };
    let build = |fee| {
        create_claim_fork_sweep(
            &source,
            ChainId::BitcoinBlake2b,
            &secp256k1::Secp256k1::verification_only(),
            &mut Getter(previous.clone()),
            &[coin],
            20.into(),
            fee,
            absolute::LockTime::ZERO,
        )
        .unwrap()
    };
    let sweep = build(3);
    assert!(matches!(
        c.prepare_fork_sweep(&context(), &sweep, policy(), 10000),
        Err(Error::Unchecked)
    ));
    for depth in [1, 5] {
        let obs = real_observation(&c, depth);
        refresh(&mut c, obs, 10000);
        assert!(matches!(
            c.prepare_fork_sweep(&context(), &sweep, policy(), 10000),
            Err(Error::Unchecked)
        ));
        assert!(c.recorded_fork_sweep().is_none());
    }
    let obs = real_observation(&c, 6);
    refresh(&mut c, obs, 10000);
    assert!(matches!(
        c.prepare_fork_sweep(&context(), &sweep, policy(), 10061),
        Err(Error::Unchecked)
    ));
    let obs = real_observation(&c, 6);
    refresh(&mut c, obs, 10000);
    c.prepare_fork_sweep(&context(), &sweep, policy(), 10000)
        .unwrap();
    assert_eq!(c.recorded_fork_sweep(), Some(&sweep.psbt().unsigned_tx));
    assert_eq!(c.recorded_fork_change_index(), Some(20.into()));
    assert_eq!(c.intent.version, 6);
    assert!(c.fresh.is_none());
    // Older v2 records remain readable, but acquire a derivation hint only
    // after authenticated construction and fresh depth checks.
    c.intent.version = 2;
    c.intent.bitcoin_transaction = None;
    c.intent.bitcoin_attempts.clear();
    c.intent.fork_change_index = None;
    c.intent.bitcoin_change_index = None;
    validate(&c.intent).unwrap();
    c.journal.store(&c.intent).unwrap();
    assert!(c.recorded_fork_change_index().is_none());
    assert!(matches!(
        c.prepare_fork_sweep(&context(), &sweep, policy(), 10000),
        Err(Error::Unchecked)
    ));
    let obs = real_observation(&c, 6);
    refresh(&mut c, obs, 10000);
    c.prepare_fork_sweep(&context(), &sweep, policy(), 10000)
        .unwrap();
    assert_eq!(c.intent.version, 3);
    let mut bad = c.intent.clone();
    bad.fork_change_index = None;
    assert!(matches!(validate(&bad), Err(Error::InvalidPlan)));
    bad.fork_change_index = Some(0x8000_0000);
    assert!(matches!(validate(&bad), Err(Error::InvalidPlan)));
    bad.version = 2;
    bad.fork_change_index = Some(20);
    assert!(matches!(validate(&bad), Err(Error::InvalidPlan)));
    let obs = real_observation(&c, 6);
    refresh(&mut c, obs, 10000);
    assert!(matches!(
        c.prepare_fork_sweep(&context(), &build(4), policy(), 10000),
        Err(Error::Conflict)
    ));
    drop(c);
    let mut c = reopen_settled(&temp.0, &wallet, context()).unwrap();
    assert_eq!(c.recorded_fork_sweep(), Some(&sweep.psbt().unsigned_tx));
    assert_eq!(c.recorded_fork_change_index(), Some(20.into()));
    assert_eq!(c.status(), Status::Unchecked);
    let obs = real_observation(&c, 6);
    refresh(&mut c, obs, 10000);
    assert!(matches!(
        c.prepare_fork_sweep(&context(), &sweep, policy(), 10000),
        Err(Error::Unchecked)
    ));
    c.revalidate_construction(&context(), &source).unwrap();
    let obs = real_observation(&c, 6);
    refresh(&mut c, obs, 10000);
    c.prepare_fork_sweep(&context(), &sweep, policy(), 10000)
        .unwrap();
    let obs = real_observation(&c, 0);
    refresh(&mut c, obs, 10000);
    assert!(matches!(
        c.prepare_fork_sweep(&context(), &sweep, policy(), 10000),
        Err(Error::Unchecked)
    ));
    assert_eq!(c.recorded_fork_sweep(), Some(&sweep.psbt().unsigned_tx));
    let sign_sweep = |sweep: &coincube_core::claim_spend::ClaimForkSweep| {
        let secp = secp256k1::Secp256k1::new();
        let mut psbt = sweep.psbt().clone();
        for b in 40..42 {
            let signer = MasterSigner::from_mnemonic(
                Network::Bitcoin,
                Mnemonic::from_entropy(&[b; 16]).unwrap(),
            )
            .unwrap();
            psbt = signer.sign_psbt(psbt, &secp).unwrap();
        }
        coincube_core::claim_finalize::finalize_claim_fork_sweep(
            sweep,
            &coincube_core::psbt_unified::UnifiedPsbt::from_psbt(psbt).unwrap(),
            &secp,
        )
        .unwrap()
    };
    let signed = sign_sweep(&sweep);
    // The last refresh observed a reorg: valid signatures cannot override it.
    assert!(matches!(
        c.record_fork_broadcast_intent(&context(), &signed, policy(), 10000),
        Err(Error::Unchecked)
    ));
    let obs = real_observation(&c, 5);
    refresh(&mut c, obs, 10000);
    assert!(matches!(
        c.record_fork_broadcast_intent(&context(), &signed, policy(), 10000),
        Err(Error::Unchecked)
    ));
    let obs = real_observation(&c, 6);
    refresh(&mut c, obs, 10000);
    assert!(matches!(
        c.record_fork_broadcast_intent(&context(), &signed, policy(), 10061),
        Err(Error::Unchecked)
    ));
    let obs = real_observation(&c, 6);
    refresh(&mut c, obs, 10000);
    let replacement = sign_sweep(&build(4));
    assert!(matches!(
        c.record_fork_broadcast_intent(&context(), &replacement, policy(), 10000),
        Err(Error::InvalidPlan)
    ));
    assert!(c.recorded_fork_submission().is_none());
    let obs = real_observation(&c, 6);
    refresh(&mut c, obs, 10000);
    c.record_fork_broadcast_intent(&context(), &signed, policy(), 10000)
        .unwrap();
    let recorded = c.recorded_fork_submission().unwrap();
    assert_eq!(recorded.txid(), signed.transaction().compute_txid());
    assert_eq!(recorded.wtxid(), signed.transaction().compute_wtxid());
    assert!(c.fresh.is_none());
    drop(c);
    let mut c = reopen_settled(&temp.0, &wallet, context()).unwrap();
    assert_eq!(c.recorded_fork_submission(), Some(recorded));
    c.revalidate_construction(&context(), &source).unwrap();
    let obs = real_observation(&c, 6);
    refresh(&mut c, obs, 10000);
    assert!(matches!(
        c.record_fork_broadcast_intent(&context(), &signed, policy(), 10000),
        Err(Error::Conflict)
    ));
    let obs = real_observation(&c, 6);
    refresh(&mut c, obs, 10000);
    assert!(matches!(
        c.prepare_fork_sweep(&context(), &sweep, policy(), 10000),
        Err(Error::Conflict)
    ));
    let before_sweep = c.recorded_fork_sweep().cloned();
    let mut new_inclusion = real_observation(&c, 6);
    if let TransactionLocation::Confirmed {
        block,
        best_chain_hash_at_height,
        ..
    } = &mut new_inclusion.observations.bitcoin.location
    {
        block.hash = hash(8);
        *best_chain_hash_at_height = hash(8);
    }
    sync_transaction_observation(&mut new_inclusion);
    let ticket = c.begin_check(&context()).unwrap();
    c.acknowledge_reconfirmation(ticket, &context(), new_inclusion, policy(), 10000)
        .unwrap();
    assert_eq!(c.recorded_fork_submission(), Some(recorded));
    assert_eq!(c.recorded_fork_sweep().cloned(), before_sweep);
    let history = c.intent.inclusion_history.clone();
    // This fixture migrated through legacy v2; recovering bytes must retain
    // both completed fork submission bookkeeping and changed Bitcoin inclusion.
    assert!(c.recorded_bitcoin_transaction().is_none());
    c.bind_recovered_bitcoin_transaction(&context(), &verified)
        .unwrap();
    assert_eq!(c.intent.inclusion_history, history);
    assert_eq!(c.recorded_fork_submission(), Some(recorded));
    assert_eq!(c.recorded_fork_sweep().cloned(), before_sweep);
    assert_eq!(c.bitcoin_submission_attempts()[0].wtxid(), None);
    drop(c);
    let mut c = reopen_settled(&temp.0, &wallet, context()).unwrap();
    c.revalidate_construction(&context(), &source).unwrap();
    assert_eq!(c.recorded_fork_submission(), Some(recorded));
    assert_eq!(c.recorded_fork_sweep().cloned(), before_sweep);
    refresh(&mut c, new_inclusion, 10000);
    assert!(matches!(
        c.record_fork_broadcast_intent(&context(), &signed, policy(), 10000),
        Err(Error::Conflict)
    ));
    let history = c.intent.inclusion_history.clone();
    let absent = real_observation(&c, 0);
    let ticket = c.begin_check(&context()).unwrap();
    c.record_resubmission(
        ticket,
        &context(),
        absent,
        verified.transaction(),
        policy(),
        10000,
    )
    .unwrap();
    assert_eq!(c.phase(), Phase::Tracking);
    assert_eq!(c.recorded_fork_submission(), Some(recorded));
    assert_eq!(c.recorded_fork_sweep().cloned(), before_sweep);
    assert_eq!(c.intent.inclusion_history, history);
    assert_eq!(c.bitcoin_submission_attempts().len(), 2);
    assert_eq!(c.bitcoin_submission_attempts()[0].wtxid(), None);
    assert_eq!(
        c.bitcoin_submission_attempts()[1].wtxid(),
        Some(verified.transaction().compute_wtxid())
    );
}

fn remined() -> CollectedAssessment {
    let mut o = observation(true);
    if let TransactionLocation::Confirmed {
        block,
        best_chain_hash_at_height,
        ..
    } = &mut o.observations.bitcoin.location
    {
        block.hash = hash(8);
        *best_chain_hash_at_height = hash(8);
    }
    sync_transaction_observation(&mut o);
    o
}
fn tracked(temp: &Temp) -> Controller {
    let mut c = controller(temp);
    refresh(&mut c, observation(false), 10000);
    c.record_broadcast_intent(&context(), &signed(), policy(), 10000)
        .unwrap();
    refresh(&mut c, observation(true), 10000);
    c
}
#[test]
fn explicit_reconfirmation_preserves_history_and_never_restores_submission() {
    let temp = Temp::new();
    let mut c = tracked(&temp);
    let old = c.last_inclusion().unwrap();
    let id = c.signed_txid();
    assert_eq!(
        refresh(&mut c, remined(), 10000),
        Status::Observation(Assessment::Reorged)
    );
    let ticket = c.begin_check(&context()).unwrap();
    c.acknowledge_reconfirmation(ticket, &context(), remined(), policy(), 10000)
        .unwrap();
    assert_eq!(c.status(), Status::Unchecked);
    assert!(c.fresh.is_none());
    assert_eq!(c.signed_txid(), id);
    assert_eq!(c.phase(), Phase::Tracking);
    assert_eq!(
        c.intent.inclusion_history,
        vec![Reconfirmation {
            previous: old,
            confirmed: c.last_inclusion().unwrap()
        }]
    );
    assert!(matches!(
        c.record_broadcast_intent(&context(), &signed(), policy(), 10000),
        Err(Error::Unchecked)
    ));
    drop(c);
    let mut c = reopen_settled(&temp.0, &identity(), context()).unwrap();
    assert_eq!(c.intent.version, 6);
    assert_eq!(c.intent.inclusion_history.len(), 1);
    assert_eq!(c.status(), Status::Unchecked);
    assert_eq!(
        refresh(&mut c, remined(), 10000),
        Status::Observation(Assessment::ObservationsEligibleForPreflight)
    );
    assert!(c
        .record_broadcast_intent(&context(), &signed(), policy(), 10000)
        .is_err());
}
#[test]
fn reconfirmation_rejects_unconfirmed_stale_wrong_chain_and_changed_context() {
    let temp = Temp::new();
    let mut c = tracked(&temp);
    let original = fs::read(temp.0.join("intent.json")).unwrap();
    let mut noncanonical = remined();
    if let TransactionLocation::Confirmed {
        best_chain_hash_at_height,
        ..
    } = &mut noncanonical.observations.bitcoin.location
    {
        *best_chain_hash_at_height = hash(9);
    }
    let mut wrong_chain = remined();
    wrong_chain.observations.bitcoin.chain = ChainId::BitcoinBlake2b;
    let mut wrong_txid = remined();
    if let TransactionLocation::Confirmed { txid, .. } =
        &mut wrong_txid.observations.bitcoin.location
    {
        *txid = Txid::from_byte_array([99; 32]);
    }
    sync_transaction_observation(&mut wrong_txid);
    for (o, now) in [
        (observation(false), 10000),
        (observation(true), 10000),
        (remined(), 10061),
        (noncanonical, 10000),
        (wrong_chain, 10000),
        (wrong_txid, 10000),
    ] {
        let ticket = c.begin_check(&context()).unwrap();
        assert!(c
            .acknowledge_reconfirmation(ticket, &context(), o, policy(), now)
            .is_err());
        assert_eq!(fs::read(temp.0.join("intent.json")).unwrap(), original);
    }
    let ticket = c.begin_check(&context()).unwrap();
    let mut other = context();
    other.generation += 1;
    assert!(c
        .acknowledge_reconfirmation(ticket, &other, remined(), policy(), 10000)
        .is_err());
    assert_eq!(fs::read(temp.0.join("intent.json")).unwrap(), original);
}

#[test]
fn signed_bytes_and_attempt_are_durable_without_restart_authority() {
    let temp = Temp::new();
    let mut c = controller(&temp);
    refresh(&mut c, observation(false), 10000);
    let tx = signed();
    c.record_broadcast_intent(&context(), &tx, policy(), 10000)
        .unwrap();
    assert_eq!(c.recorded_bitcoin_transaction(), Some(&tx));
    assert_eq!(
        c.bitcoin_submission_attempts()[0].wtxid(),
        Some(tx.compute_wtxid())
    );
    drop(c);
    let c = reopen_settled(&temp.0, &identity(), context()).unwrap();
    assert_eq!(c.status(), Status::Unchecked);
    assert!(!c.construction_verified);
    assert_eq!(c.recorded_bitcoin_transaction(), Some(&tx));
    assert_eq!(c.bitcoin_submission_attempts().len(), 1);
    for mutation in 0..7 {
        let mut bad = c.intent.clone();
        match mutation {
            0 => bad.bitcoin_transaction = None,
            1 => bad.bitcoin_attempts.clear(),
            2 => bad.bitcoin_transaction.as_mut().unwrap().input[0]
                .witness
                .clear(),
            3 => bad.bitcoin_transaction.as_mut().unwrap().output[0].value += Amount::ONE_SAT,
            4 => bad.bitcoin_attempts[0].wtxid = Some(Wtxid::from_byte_array([42; 32])),
            5 => bad
                .bitcoin_attempts
                .push(BitcoinSubmissionAttempt { wtxid: None }),
            _ => bad.bitcoin_attempts = vec![bad.bitcoin_attempts[0]; 129],
        }
        assert!(validate(&bad).is_err(), "mutation {}", mutation);
    }
}

#[test]
fn legacy_recovered_witness_does_not_rewrite_unknown_original_attempt() {
    let temp = Temp::new();
    let (built, verified) = real_artifact(ChainId::Bitcoin, false, 10);
    let mut c =
        Controller::create(&temp.0, "btc".into(), "fork".into(), &built, context()).unwrap();
    let observation = real_observation(&c, 0);
    refresh(&mut c, observation, 10000);
    c.record_broadcast_intent(&context(), verified.transaction(), policy(), 10000)
        .unwrap();
    c.intent.version = 4;
    c.intent.bitcoin_transaction = None;
    c.intent.bitcoin_attempts.clear();
    validate(&c.intent).unwrap();
    c.journal.store(&c.intent).unwrap();
    let identity = c.identity().clone();
    drop(c);
    let mut c = reopen_settled(&temp.0, &identity, context()).unwrap();
    assert!(c
        .bind_recovered_bitcoin_transaction(&context(), &verified)
        .is_err());
    c.revalidate_construction(&context(), &built).unwrap();
    c.bind_recovered_bitcoin_transaction(&context(), &verified)
        .unwrap();
    assert_eq!(c.intent.version, 6);
    assert_eq!(c.bitcoin_submission_attempts().len(), 1);
    assert_eq!(c.bitcoin_submission_attempts()[0].wtxid(), None);
    assert_eq!(c.status(), Status::Unchecked);
    let before = fs::read(temp.0.join("intent.json")).unwrap();
    c.bind_recovered_bitcoin_transaction(&context(), &verified)
        .unwrap();
    assert_eq!(fs::read(temp.0.join("intent.json")).unwrap(), before);
    let (_, other) = real_artifact(ChainId::Bitcoin, false, 11);
    assert!(matches!(
        c.bind_recovered_bitcoin_transaction(&context(), &other),
        Err(Error::Conflict)
    ));
    assert_eq!(fs::read(temp.0.join("intent.json")).unwrap(), before);
    drop(c);
    let c = reopen_settled(&temp.0, &identity, context()).unwrap();
    assert_eq!(c.bitcoin_submission_attempts()[0].wtxid(), None);
    assert_eq!(
        c.recorded_bitcoin_transaction(),
        Some(verified.transaction())
    );
}

#[cfg(unix)]
#[test]
fn preparing_existing_journal_directory_never_follows_symlinks_or_repairs_permissions() {
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
    let parent = Temp::new();
    let target = parent.0.join("target");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    let link = parent.0.join("claim-link");
    symlink(&target, &link).unwrap();
    assert!(matches!(
        prepare_directory(&link),
        Err(Error::InvalidJournal)
    ));
    assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o755);
    assert!(matches!(
        prepare_directory(&target),
        Err(Error::InvalidJournal)
    ));
    assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o755);
    let private = parent.0.join("new-wallet").join("claim");
    prepare_directory(&private).unwrap();
    assert_eq!(fs::metadata(&private).unwrap().mode() & 0o777, 0o700);
    prepare_directory(&private).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_journal_pins_directory_and_rejects_hard_links() {
    let temp = Temp::new();
    let c = controller(&temp);
    assert!(fs::rename(&temp.0, temp.0.with_extension("moved")).is_err());
    drop(c);
    let alias = temp.0.join("alias.json");
    fs::hard_link(temp.0.join("intent.json"), &alias).unwrap();
    assert!(Controller::reopen(&temp.0, &identity(), context()).is_err());
    fs::remove_file(alias).unwrap();
    assert!(Controller::reopen(&temp.0, &identity(), context()).is_ok());
}

#[test]
fn journal_crash_child() {
    let Some(directory) = std::env::var_os("COINCUBE_TEST_CLAIM_CRASH_DIRECTORY") else {
        return;
    };
    // Exercise storage directly; a reopened controller correctly has no
    // construction authority, and this fixture must not bypass that guard.
    let mut journal = Journal::open(std::path::Path::new(&directory)).unwrap();
    let mut intent = journal.load().unwrap().unwrap();
    intent.phase = Phase::BroadcastUncertain;
    intent.signed_txid = Some(signed().compute_txid());
    journal.store(&intent).unwrap();
    panic!("child did not stop at the selected storage boundary");
}

#[test]
fn process_crash_releases_lock_and_recovers_only_complete_intents() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
        sync::mpsc,
        time::Duration,
    };
    for stage in ["created", "flushed", "replaced"] {
        let temp = Temp::new();
        drop(controller(&temp));
        let directory = fs::canonicalize(&temp.0).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "services::claim_workflow::tests::journal_crash_child",
                "--nocapture",
            ])
            .env("COINCUBE_TEST_CLAIM_CRASH_DIRECTORY", &directory)
            .env("COINCUBE_TEST_CLAIM_CRASH_STAGE", stage)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, receiver) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if line.unwrap().contains("CLAIM_WRITE_BOUNDARY_REACHED") {
                    let _ = sender.send(());
                    break;
                }
            }
        });
        let reached = receiver.recv_timeout(Duration::from_secs(30));
        // Kill before assertions so a broken fixture never leaves a paused child.
        let _ = child.kill();
        let status = child.wait().unwrap();
        reader.join().unwrap();
        assert!(reached.is_ok(), "child did not reach {}: {}", stage, status);
        assert!(!status.success());
        let mut recovered = reopen_settled(&directory, &identity(), context()).unwrap();
        assert_eq!(recovered.status(), Status::Unchecked);
        assert_eq!(
            recovered.phase(),
            if stage == "replaced" {
                Phase::BroadcastUncertain
            } else {
                Phase::Intent
            }
        );
        assert!(recovered
            .record_broadcast_intent(&context(), &signed(), policy(), 10000)
            .is_err());
        // Temporary files are never mistaken for a committed intent, nor does
        // a recovered record restore signing/submission authority.
        if stage == "replaced" {
            assert_eq!(recovered.signed_txid(), Some(signed().compute_txid()));
        }
    }
}

#[cfg(windows)]
#[test]
fn windows_replacement_failure_poisons_owner_without_changing_saved_intent() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = Temp::new();
    let mut c = controller(&temp);
    let path = temp.0.join("intent.json");
    refresh(&mut c, observation(false), 10000);
    let before = fs::read(&path).unwrap();
    let blocker = fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)
        .unwrap();
    assert!(matches!(
        c.record_broadcast_intent(&context(), &signed(), policy(), 10000),
        Err(Error::Io(_))
    ));
    assert_eq!(c.status(), Status::Unchecked);
    drop(blocker);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(matches!(
        c.journal.store(&c.intent.clone()),
        Err(Error::InvalidJournal)
    ));
    drop(c);
    let recovered = Controller::reopen(&temp.0, &identity(), context()).unwrap();
    assert_eq!(recovered.phase(), Phase::Intent);
    assert_eq!(recovered.status(), Status::Unchecked);
}

mod ancestry;
