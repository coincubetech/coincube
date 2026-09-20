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
        fs::create_dir(&p).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&p, fs::Permissions::from_mode(0o700)).unwrap();
        }
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
    Controller::create_intent(&temp.0, identity(), plan(), context()).unwrap()
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
    let mut c = Controller::reopen(&temp.0, &identity(), context()).unwrap();
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
    let mut c = Controller::reopen(&temp.0, &identity(), context()).unwrap();
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
    assert!(matches!(
        Controller::reopen(&temp.0, &wrong, context()),
        Err(Error::WrongIdentity)
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            temp.0.join("intent.json"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        assert!(matches!(
            Controller::reopen(&temp.0, &identity(), context()),
            Err(Error::InvalidJournal)
        ));
    }
}
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
}

#[test]
fn restart_rejects_different_account_or_provider_but_not_new_generation() {
    let temp = Temp::new();
    drop(controller(&temp));
    let mut changed = context();
    changed.account.push('x');
    assert!(matches!(
        Controller::reopen(&temp.0, &identity(), changed),
        Err(Error::WrongIdentity)
    ));
    let mut changed = context();
    changed.provider.push('x');
    assert!(matches!(
        Controller::reopen(&temp.0, &identity(), changed),
        Err(Error::WrongIdentity)
    ));
    let mut renewed = context();
    renewed.generation += 1;
    assert_eq!(
        Controller::reopen(&temp.0, &identity(), renewed)
            .unwrap()
            .status(),
        Status::Unchecked
    );
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
    let mut c = Controller::reopen(&temp.0, &identity, context()).unwrap();
    assert!(!c.construction_verified);
    assert!(matches!(
        c.revalidate_construction(&context(), &artifact(11)),
        Err(Error::WrongIdentity)
    ));
    c.revalidate_construction(&context(), &built).unwrap();
    assert!(c.construction_verified);
    assert_eq!(c.status(), Status::Unchecked);
}

#[test]
fn corrupted_unsigned_bytes_are_not_restored_as_valid_intent() {
    let temp = Temp::new();
    drop(controller(&temp));
    let path = temp.0.join("intent.json");
    let mut intent: Intent = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    intent.plan.step1.output[0].value = Amount::from_sat(1);
    fs::write(&path, serde_json::to_vec(&intent).unwrap()).unwrap();
    assert!(matches!(
        Controller::reopen(&temp.0, &identity(), context()),
        Err(Error::InvalidPlan)
    ));
}
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
    assert!(Controller::reopen(&temp.0, &identity(), context()).is_err());
    assert_eq!(fs::read(target).unwrap(), bytes);
}
