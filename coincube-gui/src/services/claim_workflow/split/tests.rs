//! Split (#568 B0) journal tests. Real step-1 constructions from the core
//! builder, signed by rust-bitcoin's reference PSBT signer and verified by
//! `finalize_split_step1`; observations are synthetic bundles.
use super::*;
use crate::services::claim_observation::TransactionObservation;
use coincube_core::{
    claim::{
        BitcoinObservation, DeploymentObservation, DeploymentState, ForkObservation,
        ForkTransactionPresence, PreflightTips,
    },
    foreign_split::{
        create_split_step1, finalize_split_step1, SplitBranch, SplitCoin, SplitInputs,
    },
    miniscript::bitcoin::{
        absolute::LockTime,
        bip32::{DerivationPath, Xpriv, Xpub},
        ecdsa,
        psbt::Psbt,
        secp256k1::{self, Secp256k1},
        sighash::{EcdsaSighashType, SighashCache},
        transaction, Amount, BlockHash, Network, OutPoint, TxIn, TxOut, Txid,
    },
};
use std::{fs, path::PathBuf};

const FORK: u64 = 900;
const TIP: u32 = 960;
const TARGET: &str = "btcb2-target-cube";

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "split-journal-{}-{}",
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
fn policy() -> Policy {
    Policy {
        max_observation_age_seconds: 60,
        expiry_margin_seconds: 600,
    }
}
fn hash(n: u8) -> BlockHash {
    BlockHash::from_byte_array([n; 32])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Wpkh,
    ShWpkh,
    Pkh,
    WshMulti,
}

fn master(seed: u8) -> Xpriv {
    Xpriv::new_master(Network::Bitcoin, &[seed; 32]).unwrap()
}
fn account(master: &Xpriv, path: &str) -> String {
    let secp = Secp256k1::new();
    let child = master
        .derive_priv(&secp, &DerivationPath::from_str(path).unwrap())
        .unwrap();
    format!(
        "[{}/{}]{}",
        master.fingerprint(&secp),
        path.trim_start_matches("m/"),
        Xpub::from_priv(&secp, &child)
    )
}

struct Wallet {
    source: SplitSource,
    signers: Vec<Xpriv>,
}
fn make_wallet(shape: Shape, seed: u8) -> Wallet {
    let (template, signers) = match shape {
        Shape::Wpkh => (
            format!("wpkh({}/{{b}}/*)", account(&master(seed), "m/84'/0'/0'")),
            vec![master(seed)],
        ),
        Shape::ShWpkh => (
            format!(
                "sh(wpkh({}/{{b}}/*))",
                account(&master(seed), "m/49'/0'/0'")
            ),
            vec![master(seed)],
        ),
        Shape::Pkh => (
            format!("pkh({}/{{b}}/*)", account(&master(seed), "m/44'/0'/0'")),
            vec![master(seed)],
        ),
        Shape::WshMulti => {
            let masters = [master(seed), master(seed + 1), master(seed + 2)];
            let keys: Vec<_> = masters
                .iter()
                .map(|m| format!("{}/{{b}}/*", account(m, "m/48'/0'/0'/2'")))
                .collect();
            (
                format!("wsh(multi(2,{}))", keys.join(",")),
                masters.to_vec(),
            )
        }
    };
    let branch = |b: u32| Descriptor::from_str(&template.replace("{b}", &b.to_string())).unwrap();
    Wallet {
        source: SplitSource::new(branch(0), Some(branch(1))).unwrap(),
        signers,
    }
}

fn coin(source: &SplitSource, branch: SplitBranch, index: u32, sats: u64) -> SplitCoin {
    let descriptor = match branch {
        SplitBranch::External => source.external(),
        SplitBranch::Internal => source.internal().unwrap(),
    };
    let previous = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([index as u8 + 1; 32]), 0),
            ..TxIn::default()
        }],
        output: vec![TxOut {
            value: Amount::from_sat(sats),
            script_pubkey: descriptor
                .at_derivation_index(index)
                .unwrap()
                .script_pubkey(),
        }],
    };
    let block = BlockRef {
        height: FORK - 10,
        hash: hash(0x33),
    };
    SplitCoin {
        outpoint: OutPoint::new(previous.compute_txid(), 0),
        branch,
        index,
        previous,
        bitcoin_block: Some(block),
        btcb2_block: Some(block),
    }
}

fn construction_at(wallet: &Wallet, feerate: u64) -> SplitStep1 {
    let coins = [
        coin(&wallet.source, SplitBranch::External, 0, 150_000),
        coin(&wallet.source, SplitBranch::Internal, 1, 70_000),
    ];
    create_split_step1(
        &SplitInputs {
            chain: ChainId::Bitcoin,
            source: &wallet.source,
            coins: &coins,
            fork_height: FORK,
            destination: 5,
        },
        feerate,
        LockTime::from_height(TIP).unwrap(),
        TIP,
        hash(7),
    )
    .unwrap()
}

/// Two signers for multisig (the first two keys), one otherwise.
fn sign(step1: &SplitStep1, signers: &[Xpriv]) -> VerifiedSplitStep1 {
    let secp = Secp256k1::new();
    let mut psbt = step1.psbt().clone();
    for signer in signers.iter().take(2) {
        psbt.sign(signer, &secp).unwrap();
    }
    finalize_split_step1(step1, &psbt, &secp).unwrap()
}

/// A second valid ECDSA signature over the same legacy (P2PKH) sighash with
/// different nonce data: another signed transaction with another txid.
fn resign_pkh(step1: &SplitStep1, signer: &Xpriv, nonce: u8) -> VerifiedSplitStep1 {
    let secp = Secp256k1::new();
    let mut psbt: Psbt = step1.psbt().clone();
    for index in 0..psbt.inputs.len() {
        let previous = psbt.inputs[index].non_witness_utxo.clone().unwrap();
        let vout = psbt.unsigned_tx.input[index].previous_output.vout as usize;
        let script = previous.output[vout].script_pubkey.clone();
        let sighash = SighashCache::new(&psbt.unsigned_tx)
            .legacy_signature_hash(index, &script, EcdsaSighashType::All.to_u32())
            .unwrap();
        let (public, (_, path)) = psbt.inputs[index]
            .bip32_derivation
            .iter()
            .next()
            .map(|(k, v)| (*k, v.clone()))
            .unwrap();
        let key = signer.derive_priv(&secp, &path).unwrap().private_key;
        assert_eq!(key.public_key(&secp), public);
        let signature = secp.sign_ecdsa_with_noncedata(
            &secp256k1::Message::from_digest(sighash.to_byte_array()),
            &key,
            &[nonce; 32],
        );
        psbt.inputs[index].partial_sigs.insert(
            coincube_core::miniscript::bitcoin::PublicKey::new(public),
            ecdsa::Signature {
                signature,
                sighash_type: EcdsaSighashType::All,
            },
        );
    }
    finalize_split_step1(step1, &psbt, &secp).unwrap()
}

fn setup(shape: Shape) -> (Wallet, SplitStep1, VerifiedSplitStep1) {
    let wallet = make_wallet(shape, 1);
    let step1 = construction_at(&wallet, 2);
    let signed = sign(&step1, &wallet.signers);
    (wallet, step1, signed)
}
fn create(temp: &Temp, step1: &SplitStep1, signed: &VerifiedSplitStep1) -> Controller {
    Controller::create_split(&temp.0, TARGET.into(), step1, signed, FORK, context()).unwrap()
}
fn identity(step1: &SplitStep1) -> WalletIdentity {
    split_identity(TARGET.into(), step1.source().digest())
}
fn reopen(temp: &Temp, step1: &SplitStep1) -> Result<Controller, Error> {
    Controller::reopen_settling_blocking(&temp.0, &identity(step1), context())
}

#[derive(Clone, Copy)]
enum Bitcoin {
    Absent,
    Confirmed { depth: u64 },
}
/// A fresh bundle about `txid` on both chains. Never present on BTCB2.
fn observation(txid: Txid, bitcoin: Bitcoin) -> CollectedAssessment {
    let block = BlockRef {
        height: 100,
        hash: hash(4),
    };
    let tip = BlockRef {
        height: match bitcoin {
            Bitcoin::Absent => 105,
            Bitcoin::Confirmed { depth } => 99 + depth,
        },
        hash: hash(1),
    };
    let fork = BlockRef {
        height: 100,
        hash: hash(2),
    };
    let (transaction, location) = match bitcoin {
        Bitcoin::Absent => (
            TransactionObservation::Absent,
            TransactionLocation::Unconfirmed,
        ),
        Bitcoin::Confirmed { .. } => (
            TransactionObservation::Confirmed { txid, block },
            TransactionLocation::Confirmed {
                txid,
                block,
                best_chain_hash_at_height: block.hash,
            },
        ),
    };
    CollectedAssessment {
        generation: 1,
        assessment: Assessment::Unknown,
        observations: ObservationBundle {
            bitcoin_transaction: transaction,
            bitcoin: BitcoinObservation {
                chain: ChainId::Bitcoin,
                tip,
                location,
                observed_at: 10_000,
            },
            fork: ForkObservation {
                chain: ChainId::BitcoinBlake2b,
                step1_txid: txid,
                step1_presence: ForkTransactionPresence::NotObserved,
                tip: fork,
                median_time_past: 8_000,
                observed_at: 10_000,
            },
            deployment: DeploymentObservation {
                chain: ChainId::BitcoinBlake2b,
                tip: fork,
                state: DeploymentState::Flagday {
                    height: 90,
                    expiry_time: 20_000,
                    active: true,
                },
                observed_at: 10_000,
            },
            preflight: PreflightTips { bitcoin: tip, fork },
        },
    }
}
fn refresh(c: &mut Controller, o: CollectedAssessment) -> Status {
    let ticket = c.begin_check(&context()).unwrap();
    c.apply_observation(ticket, &context(), Ok(o), policy(), 10_000)
        .unwrap()
}
/// The journal's entry point, which dispatches Split intents to their rules.
fn full_validate(intent: &Intent) -> Result<(), Error> {
    super::super::validate(intent)
}
/// A minimal Claim intent (native segwit, OP_RETURN poison).
fn claim_intent(directory: &std::path::Path) -> Controller {
    let prevout = OutPoint::new(Txid::from_byte_array([3; 32]), 0);
    let mut poison = vec![0x6a, 0x4c, 87];
    poison.extend_from_slice(&[1; 87]);
    Controller::create_intent(
        directory,
        WalletIdentity {
            bitcoin_cube: "btc-fixture".into(),
            fork_cube: "fork-fixture".into(),
            descriptor_digest: sha256::Hash::hash(b"synthetic-descriptor"),
        },
        ClaimPlan {
            bitcoin_chain: ChainId::Bitcoin,
            fork_chain: ChainId::BitcoinBlake2b,
            step1: Transaction {
                version: transaction::Version::TWO,
                lock_time: LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: prevout,
                    ..TxIn::default()
                }],
                output: vec![TxOut {
                    value: Amount::ZERO,
                    script_pubkey: ScriptBuf::from_bytes(poison),
                }],
            },
            claimed_prevouts: vec![prevout],
            poison: Poison::OpReturn,
            previous_confirmation: None,
            tracked_txid: None,
        },
        context(),
        None,
    )
    .unwrap()
}
fn journal_text(temp: &Temp) -> String {
    fs::read_to_string(temp.0.join("intent.json")).unwrap()
}
#[cfg(unix)]
fn mode(temp: &Temp) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(temp.0.join("intent.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}
/// Submit the recorded step 1 in the journal only: a fresh absence check
/// keyed by the signed txid, then the durable intent.
fn record(c: &mut Controller, signed: &VerifiedSplitStep1) {
    let txid = signed.transaction().compute_txid();
    assert_eq!(
        refresh(c, observation(txid, Bitcoin::Absent)),
        Status::Observation(Assessment::WaitingForConfirmation)
    );
    c.record_split_broadcast_intent(&context(), signed, policy(), 10_000)
        .unwrap();
}

/// (b) P2PKH and P2SH-P2WPKH scriptSigs change the txid. The journal tracks
/// the signed txid, and observations keyed by the unsigned txid (the
/// counterfactual) never assess; for native segwit the two are equal.
#[test]
fn split_tracks_the_signed_txid_of_scriptsig_inputs() {
    for shape in [Shape::Pkh, Shape::ShWpkh, Shape::Wpkh, Shape::WshMulti] {
        let (_, step1, signed) = setup(shape);
        let unsigned = step1.txid();
        let tracked = signed.transaction().compute_txid();
        assert_eq!(
            unsigned != tracked,
            matches!(shape, Shape::Pkh | Shape::ShWpkh),
            "{shape:?}"
        );
        let temp = Temp::new();
        let mut c = create(&temp, &step1, &signed);
        assert_eq!(c.plan().step1_txid(), tracked, "{shape:?}");
        assert_eq!(c.plan().step1.compute_txid(), unsigned, "{shape:?}");
        assert_eq!(c.phase(), Phase::Intent);
        assert_eq!(c.signed_txid(), None);
        if unsigned != tracked {
            // The counterfactual: a view keyed by the unsigned txid is not
            // about this transaction, so nothing can be recorded from it.
            assert_eq!(
                refresh(&mut c, observation(unsigned, Bitcoin::Absent)),
                Status::Observation(Assessment::Unknown),
                "{shape:?}"
            );
            assert!(matches!(
                c.record_split_broadcast_intent(&context(), &signed, policy(), 10_000),
                Err(Error::Unchecked)
            ));
            assert_eq!(
                refresh(
                    &mut c,
                    observation(unsigned, Bitcoin::Confirmed { depth: 6 })
                ),
                Status::Observation(Assessment::Unknown),
                "{shape:?}"
            );
        }
        record(&mut c, &signed);
        assert_eq!(c.phase(), Phase::BroadcastUncertain);
        assert_eq!(c.signed_txid(), Some(tracked));
        assert_eq!(
            c.bitcoin_submission_attempts()
                .iter()
                .map(|a| a.wtxid())
                .collect::<Vec<_>>(),
            vec![Some(signed.transaction().compute_wtxid())]
        );
        assert_eq!(
            refresh(
                &mut c,
                observation(tracked, Bitcoin::Confirmed { depth: 6 })
            ),
            Status::Observation(Assessment::ObservationsEligibleForPreflight),
            "{shape:?}"
        );
        assert_eq!(c.phase(), Phase::Tracking);
        assert!(c.last_inclusion().is_some());
        drop(c);
        let c = reopen(&temp, &step1).unwrap();
        assert_eq!(c.plan().step1_txid(), tracked);
        assert_eq!(c.recorded_bitcoin_transaction(), Some(signed.transaction()));
    }
}

/// The journal is version 8 in the owner-only file, carries no Bitcoin
/// Cube, the public descriptors (P2), and the signature only in the signed
/// transaction.
#[test]
fn split_journal_is_a_private_version_8_record_without_a_bitcoin_cube() {
    let (wallet, step1, signed) = setup(Shape::Pkh);
    let temp = Temp::new();
    let c = create(&temp, &step1, &signed);
    #[cfg(unix)]
    assert_eq!(mode(&temp), 0o600);
    let json: serde_json::Value = serde_json::from_str(&journal_text(&temp)).unwrap();
    assert_eq!(json["version"], 8);
    assert_eq!(json["identity"]["bitcoin_cube"], "");
    assert_eq!(json["identity"]["fork_cube"], TARGET);
    assert_eq!(json["split"]["target_cube"], TARGET);
    assert_eq!(json["split"]["fork_height"], FORK);
    assert_eq!(json["split"]["destination"], 5);
    assert_eq!(
        json["split"]["descriptors"]["external"],
        wallet.source.external().to_string()
    );
    assert!(json["split"].get("target_index").is_none());
    assert!(json["split"].get("target_script").is_none());
    assert_eq!(
        json["plan"]["tracked_txid"],
        signed.transaction().compute_txid().to_string()
    );
    for input in json["plan"]["step1"]["input"].as_array().unwrap() {
        assert_eq!(input["script_sig"], "");
    }
    let recorded = c.recorded_split().unwrap().unwrap();
    assert_eq!(recorded.source.as_ref(), Some(&wallet.source));
    assert_eq!(recorded.source_digest, wallet.source.digest());
    assert_eq!((recorded.fork_height, recorded.destination), (FORK, 5));
    assert_eq!(c.identity(), &identity(&step1));
}

/// Claim intents are untouched by the Split fields: they serialize without
/// `split` or `tracked_txid`, and a Claim identity never opens a Split
/// journal (nor the reverse).
#[test]
fn claim_journals_carry_no_split_fields_and_identities_do_not_cross() {
    let temp = Temp::new();
    let claim = claim_intent(&temp.0);
    let text = journal_text(&temp);
    assert!(!text.contains("split") && !text.contains("tracked_txid"));
    assert!(claim.recorded_split().unwrap().is_none());
    drop(claim);

    let (_, step1, signed) = setup(Shape::Wpkh);
    let temp = Temp::new();
    drop(create(&temp, &step1, &signed));
    let mut wrong = identity(&step1);
    wrong.bitcoin_cube = "btc-cube".into();
    assert!(matches!(
        Controller::reopen_settling_blocking(&temp.0, &wrong, context()),
        Err(Error::WrongIdentity)
    ));
    let mut wrong = identity(&step1);
    wrong.fork_cube = "another-target".into();
    assert!(matches!(
        Controller::reopen_settling_blocking(&temp.0, &wrong, context()),
        Err(Error::WrongIdentity)
    ));
    let mut c = reopen(&temp, &step1).unwrap();
    // Claim-only operations refuse a Split intent outright.
    assert!(matches!(
        c.record_broadcast_intent(&context(), signed.transaction(), policy(), 10_000),
        Err(Error::WrongIdentity)
    ));
}

/// (c) Create, restart revalidation and binding refuse a different source,
/// a changed construction, another fork height, a stale generation and a
/// mismatched signed transaction.
#[test]
fn split_create_and_revalidation_refusals() {
    let (wallet, step1, signed) = setup(Shape::Pkh);
    let temp = Temp::new();
    // A signature over another construction.
    let other = construction_at(&wallet, 3);
    let other_signed = sign(&other, &wallet.signers);
    assert!(matches!(
        Controller::create_split(
            &temp.0,
            TARGET.into(),
            &step1,
            &other_signed,
            FORK,
            context()
        ),
        Err(Error::WrongIdentity)
    ));
    assert!(matches!(
        Controller::create_split(&temp.0, String::new(), &step1, &signed, FORK, context()),
        Err(Error::InvalidPlan)
    ));
    // Not the construction's fork height (no construction can have 0: no
    // coin precedes it). Validation refuses a zero fork height as well.
    assert!(matches!(
        Controller::create_split(&temp.0, TARGET.into(), &step1, &signed, 0, context()),
        Err(Error::WrongIdentity)
    ));
    let mut no_account = context();
    no_account.account.clear();
    assert!(matches!(
        Controller::create_split(&temp.0, TARGET.into(), &step1, &signed, FORK, no_account),
        Err(Error::Revoked)
    ));
    let c = create(&temp, &step1, &signed);
    drop(c);
    // One intent per directory.
    assert!(matches!(
        Controller::create_split(&temp.0, TARGET.into(), &step1, &signed, FORK, context()),
        Err(Error::Conflict)
    ));

    let mut c = reopen(&temp, &step1).unwrap();
    // Wrong source digest: the same shape from another seed.
    let stranger = make_wallet(Shape::Pkh, 9);
    let foreign = construction_at(&stranger, 2);
    assert!(matches!(
        c.revalidate_split_construction(&context(), &foreign, FORK),
        Err(Error::WrongIdentity)
    ));
    // Changed construction: same source, another fee.
    assert!(matches!(
        c.revalidate_split_construction(&context(), &other, FORK),
        Err(Error::WrongIdentity)
    ));
    // Another fork height than recorded.
    assert!(matches!(
        c.revalidate_split_construction(&context(), &step1, FORK + 1),
        Err(Error::WrongIdentity)
    ));
    assert!(!c.construction_verified);
    // A failed revalidation leaves binding impossible.
    assert!(matches!(
        c.bind_recovered_split_transaction(&context(), &signed),
        Err(Error::Unchecked)
    ));
    c.revalidate_split_construction(&context(), &step1, FORK)
        .unwrap();
    assert!(matches!(
        c.bind_recovered_split_transaction(&context(), &other_signed),
        Err(Error::WrongIdentity)
    ));
    c.bind_recovered_split_transaction(&context(), &signed)
        .unwrap();
    // Stale generation: the controller is revoked and stays revoked.
    let mut stale = context();
    stale.generation += 1;
    assert!(matches!(
        c.revalidate_split_construction(&stale, &step1, FORK),
        Err(Error::Revoked)
    ));
    assert!(matches!(
        c.revalidate_split_construction(&context(), &step1, FORK),
        Err(Error::Revoked)
    ));
    assert!(matches!(c.begin_check(&context()), Err(Error::Revoked)));
    drop(c);

    // Wrong source digest with an identical transaction: the receive
    // descriptor alone builds the same step 1 from receive-branch coins, so
    // only the source identity tells the two apart.
    let receive_only = SplitSource::new(wallet.source.external().clone(), None).unwrap();
    let receive = |source: &SplitSource| {
        let coins = [coin(source, SplitBranch::External, 0, 150_000)];
        create_split_step1(
            &SplitInputs {
                chain: ChainId::Bitcoin,
                source,
                coins: &coins,
                fork_height: FORK,
                destination: 5,
            },
            2,
            LockTime::from_height(TIP).unwrap(),
            TIP,
            hash(7),
        )
        .unwrap()
    };
    let (paired, alone) = (receive(&wallet.source), receive(&receive_only));
    assert_eq!(paired.psbt().unsigned_tx, alone.psbt().unsigned_tx);
    assert_ne!(wallet.source.digest(), receive_only.digest());
    let temp = Temp::new();
    let paired_signed = sign(&paired, &wallet.signers);
    drop(create(&temp, &paired, &paired_signed));
    let mut c = reopen(&temp, &paired).unwrap();
    assert!(matches!(
        c.revalidate_split_construction(&context(), &alone, FORK),
        Err(Error::WrongIdentity)
    ));
    c.revalidate_split_construction(&context(), &paired, FORK)
        .unwrap();
}

/// (d) Resume is Unchecked. Once a submission was recorded, restart can
/// only reconcile: no new intent, no replacement signed bytes.
#[test]
fn split_resume_is_unchecked_and_an_uncertain_submission_only_reconciles() {
    let (wallet, step1, signed) = setup(Shape::Pkh);
    let temp = Temp::new();
    let mut c = create(&temp, &step1, &signed);
    record(&mut c, &signed);
    drop(c);
    let mut c = reopen(&temp, &step1).unwrap();
    assert_eq!(c.status(), Status::Unchecked);
    assert_eq!(c.phase(), Phase::BroadcastUncertain);
    assert!(!c.construction_verified);
    assert!(matches!(
        c.record_split_broadcast_intent(&context(), &signed, policy(), 10_000),
        Err(Error::Unchecked)
    ));
    c.revalidate_split_construction(&context(), &step1, FORK)
        .unwrap();
    c.bind_recovered_split_transaction(&context(), &signed)
        .unwrap();
    // Another valid signature cannot replace bytes that may be on the wire.
    let resigned = resign_pkh(&step1, &wallet.signers[0], 1);
    assert_ne!(
        resigned.transaction().compute_txid(),
        signed.transaction().compute_txid()
    );
    assert!(matches!(
        c.bind_recovered_split_transaction(&context(), &resigned),
        Err(Error::Conflict)
    ));
    // A fresh absence still records nothing new: only reconciliation.
    let txid = signed.transaction().compute_txid();
    assert_eq!(
        refresh(&mut c, observation(txid, Bitcoin::Absent)),
        Status::Observation(Assessment::WaitingForConfirmation)
    );
    assert!(matches!(
        c.record_split_broadcast_intent(&context(), &signed, policy(), 10_000),
        Err(Error::Unchecked)
    ));
    assert_eq!(c.bitcoin_submission_attempts().len(), 1);
    assert_eq!(
        refresh(&mut c, observation(txid, Bitcoin::Confirmed { depth: 2 })),
        Status::Observation(Assessment::WaitingForDepth { confirmations: 2 })
    );
    assert_eq!(c.phase(), Phase::Tracking);
}

/// Before any submission a re-signed step 1 replaces the recorded one and
/// becomes the tracked txid; nothing was ever sent under the old one.
#[test]
fn split_resigning_before_submission_moves_the_tracked_txid() {
    let (wallet, step1, signed) = setup(Shape::Pkh);
    let temp = Temp::new();
    drop(create(&temp, &step1, &signed));
    let mut c = reopen(&temp, &step1).unwrap();
    c.revalidate_split_construction(&context(), &step1, FORK)
        .unwrap();
    let resigned = resign_pkh(&step1, &wallet.signers[0], 2);
    c.bind_recovered_split_transaction(&context(), &resigned)
        .unwrap();
    let tracked = resigned.transaction().compute_txid();
    assert_eq!(c.plan().step1_txid(), tracked);
    // The old signed transaction is no longer the recorded one.
    assert_eq!(
        refresh(
            &mut c,
            observation(signed.transaction().compute_txid(), Bitcoin::Absent)
        ),
        Status::Observation(Assessment::Unknown)
    );
    assert!(matches!(
        c.record_split_broadcast_intent(&context(), &signed, policy(), 10_000),
        Err(Error::Unchecked)
    ));
    assert_eq!(
        refresh(&mut c, observation(tracked, Bitcoin::Absent)),
        Status::Observation(Assessment::WaitingForConfirmation)
    );
    assert!(matches!(
        c.record_split_broadcast_intent(&context(), &signed, policy(), 10_000),
        Err(Error::InvalidPlan)
    ));
    record(&mut c, &resigned);
    assert_eq!(c.signed_txid(), Some(tracked));
    drop(c);
    assert_eq!(reopen(&temp, &step1).unwrap().plan().step1_txid(), tracked);
}

/// A Pkh construction built at `fork_height`. The fork height only bounds
/// which coins are pre-fork, so every height above the coins' block builds
/// the identical transaction.
fn built_at(wallet: &Wallet, fork_height: u64) -> SplitStep1 {
    let coins = [
        coin(&wallet.source, SplitBranch::External, 0, 150_000),
        coin(&wallet.source, SplitBranch::Internal, 1, 70_000),
    ];
    create_split_step1(
        &SplitInputs {
            chain: ChainId::Bitcoin,
            source: &wallet.source,
            coins: &coins,
            fork_height,
            destination: 5,
        },
        2,
        LockTime::from_height(TIP).unwrap(),
        TIP,
        hash(7),
    )
    .unwrap()
}

/// CodeRabbit on #622: `create_split` must journal the fork height the
/// construction was built with, not whatever the caller passes.
#[test]
fn split_create_refuses_a_fork_height_other_than_the_constructions() {
    let wallet = make_wallet(Shape::Pkh, 1);
    let (step1, other) = (built_at(&wallet, FORK), built_at(&wallet, FORK + 50));
    assert_eq!(step1.psbt().unsigned_tx, other.psbt().unsigned_tx);
    let signed = sign(&step1, &wallet.signers);
    let temp = Temp::new();
    assert!(matches!(
        Controller::create_split(&temp.0, TARGET.into(), &step1, &signed, FORK + 1, context()),
        Err(Error::WrongIdentity)
    ));
    assert!(matches!(
        Controller::create_split(&temp.0, TARGET.into(), &other, &signed, FORK, context()),
        Err(Error::WrongIdentity)
    ));
    assert!(!temp.0.join("intent.json").exists());
    drop(create(&temp, &step1, &signed));
    let recorded = reopen(&temp, &step1)
        .unwrap()
        .recorded_split()
        .unwrap()
        .unwrap();
    assert_eq!(recorded.fork_height, FORK);
}

/// CodeRabbit on #622: revalidation must refuse a construction built at
/// another fork height even when the caller passes the recorded one.
#[test]
fn split_revalidation_refuses_a_construction_built_at_another_fork_height() {
    let wallet = make_wallet(Shape::Pkh, 1);
    let (step1, other) = (built_at(&wallet, FORK), built_at(&wallet, FORK + 50));
    assert_eq!(step1.psbt().unsigned_tx, other.psbt().unsigned_tx);
    let signed = sign(&step1, &wallet.signers);
    let temp = Temp::new();
    drop(create(&temp, &step1, &signed));
    let mut c = reopen(&temp, &step1).unwrap();
    assert!(matches!(
        c.revalidate_split_construction(&context(), &other, FORK),
        Err(Error::WrongIdentity)
    ));
    assert!(!c.construction_verified);
    c.revalidate_split_construction(&context(), &step1, FORK)
        .unwrap();
}

/// Review F1 (#622): an observed inclusion of the recorded step 1 keeps
/// phase Intent (no submission was recorded), but those bytes may be on
/// chain. A re-signed step 1 must not replace them, or the journal would
/// track a txid that is not on chain and could neither progress nor be
/// abandoned.
#[test]
fn split_observed_inclusion_blocks_rebinding_a_resigned_step1() {
    let (wallet, step1, signed) = setup(Shape::Pkh);
    let recorded = signed.transaction().compute_txid();
    let temp = Temp::new();
    let mut c = create(&temp, &step1, &signed);
    assert_eq!(
        refresh(
            &mut c,
            observation(recorded, Bitcoin::Confirmed { depth: 1 })
        ),
        Status::Observation(Assessment::WaitingForDepth { confirmations: 1 })
    );
    assert_eq!(c.phase(), Phase::Intent);
    assert!(c.last_inclusion().is_some());
    drop(c);
    let mut c = reopen(&temp, &step1).unwrap();
    c.revalidate_split_construction(&context(), &step1, FORK)
        .unwrap();
    let resigned = resign_pkh(&step1, &wallet.signers[0], 3);
    assert_ne!(resigned.transaction().compute_txid(), recorded);
    assert!(matches!(
        c.bind_recovered_split_transaction(&context(), &resigned),
        Err(Error::Conflict)
    ));
    assert_eq!(c.plan().step1_txid(), recorded);
    assert_eq!(c.recorded_bitcoin_transaction(), Some(signed.transaction()));
    // The recorded bytes still bind, and the journal keeps seeing them.
    c.bind_recovered_split_transaction(&context(), &signed)
        .unwrap();
    assert_eq!(
        refresh(
            &mut c,
            observation(recorded, Bitcoin::Confirmed { depth: 6 })
        ),
        Status::Observation(Assessment::ObservationsEligibleForPreflight)
    );
    drop(c);
    assert_eq!(reopen(&temp, &step1).unwrap().plan().step1_txid(), recorded);
}

/// A Split controller as created: phase Intent, construction verified, no
/// fresh observation. For the Claim-only guard tests in `claim_workflow`.
pub(in crate::services::claim_workflow) fn created_split(
    directory: &std::path::Path,
) -> Controller {
    let (_, step1, signed) = setup(Shape::Pkh);
    Controller::create_split(directory, TARGET.into(), &step1, &signed, FORK, context()).unwrap()
}

/// P2: the descriptors are deleted on abandonment (the whole intent) and at
/// completion (the descriptors only); the file stays owner-only.
#[test]
fn split_descriptors_are_deleted_on_abandon_and_completion() {
    let (wallet, step1, signed) = setup(Shape::WshMulti);
    let xpub = |text: &str| {
        wallet
            .source
            .external()
            .to_string()
            .split(['[', ']', '/', ','])
            .filter(|part| part.starts_with("xpub"))
            .any(|key| text.contains(key))
    };
    // Abandon before any submission: the intent is gone.
    let temp = Temp::new();
    let c = create(&temp, &step1, &signed);
    assert!(xpub(&journal_text(&temp)));
    let mut stale = context();
    stale.generation += 1;
    assert!(matches!(c.abandon_split(&stale), Err(Error::Revoked)));
    assert!(temp.0.join("intent.json").exists());
    let c = reopen(&temp, &step1).unwrap();
    c.abandon_split(&context()).unwrap();
    assert!(!temp.0.join("intent.json").exists());
    assert!(matches!(reopen(&temp, &step1), Err(Error::InvalidJournal)));
    // An observed inclusion before any recorded submission (the signed
    // bytes were sent from elsewhere) also keeps the record.
    let seen = Temp::new();
    let mut c = create(&seen, &step1, &signed);
    let txid = signed.transaction().compute_txid();
    refresh(&mut c, observation(txid, Bitcoin::Confirmed { depth: 1 }));
    assert_eq!(c.phase(), Phase::Intent);
    assert!(c.last_inclusion().is_some());
    assert!(matches!(c.abandon_split(&context()), Err(Error::Conflict)));
    assert!(seen.0.join("intent.json").exists());
    // The directory is reusable.
    let mut c = create(&temp, &step1, &signed);

    // Abandon after a recorded submission is refused; the record stays.
    record(&mut c, &signed);
    assert!(matches!(c.abandon_split(&context()), Err(Error::Conflict)));
    assert!(temp.0.join("intent.json").exists());
    let mut c = reopen(&temp, &step1).unwrap();
    // Completion needs a tracked step 1.
    assert!(matches!(
        c.forget_split_descriptors(&context()),
        Err(Error::Unchecked)
    ));
    let txid = signed.transaction().compute_txid();
    refresh(&mut c, observation(txid, Bitcoin::Confirmed { depth: 6 }));
    assert_eq!(c.phase(), Phase::Tracking);
    c.forget_split_descriptors(&context()).unwrap();
    let text = journal_text(&temp);
    assert!(!xpub(&text), "descriptor keys remain in the journal");
    assert!(!text.contains("descriptors"));
    #[cfg(unix)]
    assert_eq!(mode(&temp), 0o600);
    // Idempotent, and the record still opens and rebuilds from a supplied
    // source by digest.
    c.forget_split_descriptors(&context()).unwrap();
    drop(c);
    let mut c = reopen(&temp, &step1).unwrap();
    let recorded = c.recorded_split().unwrap().unwrap();
    assert!(recorded.source.is_none());
    assert_eq!(recorded.source_digest, wallet.source.digest());
    c.revalidate_split_construction(&context(), &step1, FORK)
        .unwrap();
    c.bind_recovered_split_transaction(&context(), &signed)
        .unwrap();
    // No temporary file is left beside the intent.
    let names: Vec<_> = fs::read_dir(&temp.0)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names
            .iter()
            .all(|n| n == "intent.json" || n == "claim.lock"),
        "{:?}",
        names
    );
}

/// Split-kind validation: signatures only in the recorded signed step 1,
/// the tracked txid is that transaction's, the identity is target-only and
/// the stored descriptors hash to the source digest. A fork sweep's output
/// must be the recorded target script, native P2WSH or P2TR.
#[test]
fn split_validation_rules() {
    let (_, step1, signed) = setup(Shape::ShWpkh);
    let temp = Temp::new();
    let mut c = create(&temp, &step1, &signed);
    let base = c.intent.clone();
    full_validate(&base).unwrap();
    let check = |change: &dyn Fn(&mut Intent)| {
        let mut next = base.clone();
        change(&mut next);
        full_validate(&next).err()
    };
    let signature = signed.transaction().input[0].script_sig.clone();
    assert!(!signature.is_empty());
    let other = make_wallet(Shape::Pkh, 4).source;
    for (name, change) in [
        (
            "signature in the plan",
            Box::new(|i: &mut Intent| i.plan.step1.input[0].script_sig = signature.clone())
                as Box<dyn Fn(&mut Intent)>,
        ),
        (
            "tracked unsigned txid",
            Box::new(|i: &mut Intent| i.plan.tracked_txid = Some(i.plan.step1.compute_txid())),
        ),
        (
            "no tracked txid",
            Box::new(|i: &mut Intent| i.plan.tracked_txid = None),
        ),
        (
            "unsigned recorded transaction",
            Box::new(|i: &mut Intent| {
                let tx = i.plan.step1.clone();
                i.plan.tracked_txid = Some(tx.compute_txid());
                i.bitcoin_transaction = Some(tx);
            }),
        ),
        (
            "no recorded transaction",
            Box::new(|i: &mut Intent| i.bitcoin_transaction = None),
        ),
        (
            "a Bitcoin Cube",
            Box::new(|i: &mut Intent| i.identity.bitcoin_cube = "btc".into()),
        ),
        (
            "another target",
            Box::new(|i: &mut Intent| i.identity.fork_cube = "other".into()),
        ),
        (
            "another digest",
            Box::new(|i: &mut Intent| {
                i.identity.descriptor_digest = sha256::Hash::hash(b"x");
                i.split.as_mut().unwrap().source_digest = sha256::Hash::hash(b"x");
            }),
        ),
        (
            "another wallet's descriptors",
            Box::new(move |i: &mut Intent| {
                i.split.as_mut().unwrap().descriptors = Some(StoredDescriptors::new(&other))
            }),
        ),
        (
            "non-canonical descriptor text",
            Box::new(|i: &mut Intent| {
                let d = i.split.as_mut().unwrap().descriptors.as_mut().unwrap();
                d.external = d.external.split('#').next().unwrap().to_owned();
            }),
        ),
        (
            "a duplicated claimed prevout",
            Box::new(|i: &mut Intent| {
                let first = i.plan.claimed_prevouts[0];
                i.plan.claimed_prevouts.push(first);
            }),
        ),
        ("version 7", Box::new(|i: &mut Intent| i.version = 7)),
        ("no split record", Box::new(|i: &mut Intent| i.split = None)),
        (
            "a Claim change index",
            Box::new(|i: &mut Intent| i.bitcoin_change_index = Some(0)),
        ),
        (
            "a fork chain as Bitcoin",
            Box::new(|i: &mut Intent| i.plan.bitcoin_chain = ChainId::BitcoinBlake2b),
        ),
        (
            "mismatched fork pair",
            Box::new(|i: &mut Intent| i.plan.fork_chain = ChainId::BitcoinBlake2bTestnet4),
        ),
        (
            "an attempt at phase Intent",
            Box::new(|i: &mut Intent| {
                i.bitcoin_attempts
                    .push(BitcoinSubmissionAttempt { wtxid: None })
            }),
        ),
        (
            "half a target reservation",
            Box::new(|i: &mut Intent| i.split.as_mut().unwrap().target_index = Some(3)),
        ),
        (
            "a P2WPKH target",
            Box::new(|i: &mut Intent| {
                let record = i.split.as_mut().unwrap();
                record.target_index = Some(3);
                record.target_script = Some(ScriptBuf::new_p2wpkh(
                    &coincube_core::miniscript::bitcoin::WPubkeyHash::from_byte_array([1; 20]),
                ));
            }),
        ),
    ] {
        assert!(check(&*change).is_some(), "{} was accepted", name);
    }
    // After a recorded submission: the signed txid is the tracked one.
    record(&mut c, &signed);
    refresh(
        &mut c,
        observation(
            signed.transaction().compute_txid(),
            Bitcoin::Confirmed { depth: 6 },
        ),
    );
    let tracking = c.intent.clone();
    full_validate(&tracking).unwrap();
    let mut next = tracking.clone();
    next.signed_txid = Some(next.plan.step1.compute_txid());
    assert!(full_validate(&next).is_err());
    let mut next = tracking.clone();
    next.bitcoin_attempts[0].wtxid = None;
    assert!(full_validate(&next).is_err());

    // Fork sweep: exactly the claimed prevouts into the recorded target.
    let p2wsh = ScriptBuf::new_p2wsh(
        &coincube_core::miniscript::bitcoin::WScriptHash::from_byte_array([2; 32]),
    );
    let p2tr = ScriptBuf::from_bytes([&[0x51, 0x20][..], &[3; 32]].concat());
    assert!(p2tr.is_p2tr());
    let sweep = |script: &ScriptBuf| Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: tracking
            .plan
            .claimed_prevouts
            .iter()
            .map(|prevout| TxIn {
                previous_output: *prevout,
                ..TxIn::default()
            })
            .collect(),
        output: vec![TxOut {
            value: Amount::from_sat(1_000),
            script_pubkey: script.clone(),
        }],
    };
    for target in [&p2wsh, &p2tr] {
        let mut next = tracking.clone();
        let record = next.split.as_mut().unwrap();
        record.target_index = Some(7);
        record.target_script = Some(target.clone());
        full_validate(&next).unwrap();
        next.fork_sweep = Some(sweep(target));
        full_validate(&next).unwrap();
        let other = if target == &p2wsh { &p2tr } else { &p2wsh };
        next.fork_sweep = Some(sweep(other));
        assert!(full_validate(&next).is_err());
        let mut signed_sweep = sweep(target);
        signed_sweep.input[0].witness.push([1]);
        next.fork_sweep = Some(signed_sweep);
        assert!(full_validate(&next).is_err());
    }
    // No target recorded: no sweep.
    let mut next = tracking.clone();
    next.fork_sweep = Some(sweep(&p2wsh));
    assert!(full_validate(&next).is_err());
}

/// (f) Binaries without Split refuse a version-8 journal, fail closed. The
/// v7 reader's `Intent` denies unknown fields and `split` is one; its
/// version gate admits 1–7 only, and a Claim intent claiming version 8
/// without a Split record is refused here too.
#[test]
fn v8_split_journal_is_refused_by_the_v7_reader() {
    #[allow(dead_code)]
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct V7Intent {
        version: u32,
        #[serde(default)]
        ancestry: Option<serde_json::Value>,
        identity: serde_json::Value,
        plan: serde_json::Value,
        unsigned_digest: serde_json::Value,
        context_digest: serde_json::Value,
        signed_txid: serde_json::Value,
        phase: serde_json::Value,
        #[serde(default)]
        fork_sweep: Option<serde_json::Value>,
        #[serde(default)]
        fork_change_index: Option<serde_json::Value>,
        #[serde(default)]
        bitcoin_change_index: Option<serde_json::Value>,
        #[serde(default)]
        fork_submission: Option<serde_json::Value>,
        #[serde(default)]
        inclusion_history: Option<serde_json::Value>,
        #[serde(default)]
        bitcoin_transaction: Option<serde_json::Value>,
        #[serde(default)]
        bitcoin_attempts: Option<serde_json::Value>,
    }
    let (_, step1, signed) = setup(Shape::Pkh);
    let temp = Temp::new();
    let c = create(&temp, &step1, &signed);
    let text = journal_text(&temp);
    let error = serde_json::from_str::<V7Intent>(&text).err().unwrap();
    assert!(
        error.to_string().contains("unknown field `split`"),
        "{}",
        error
    );
    // The same mirror reads a Claim journal, so it is a faithful v7 reader.
    let claim = Temp::new();
    drop(claim_intent(&claim.0));
    serde_json::from_str::<V7Intent>(&journal_text(&claim)).unwrap();
    // Version 8 without the record is not a Claim intent either.
    let mut next = c.intent.clone();
    next.split = None;
    assert!(matches!(full_validate(&next), Err(Error::InvalidPlan)));
}

/// D1: no GUI caller. The Split journal API is reached only from its own
/// module and tests (and re-exported by `claim_workflow`), and from the Split
/// coordinator (B0b). The coordinator's Split API (`SplitProduction`,
/// `Coordinator::create_split` / `resume_split`) and the daemonless
/// transport (`submit_verified_split_step1_to_connect`, `for_split_step1`)
/// are reached only from the Split coordinator and its tests, plus the one
/// gate constructor in the coordinator's step-1 dispatch. B1b adds the Split
/// step-1 panel (`app/state/vault/split/`) for the create, resume, read and
/// abandon calls only; the panel is constructed in production only to resume
/// an existing journal (`split_panel_has_no_gui_entry_point`), so nothing in
/// the GUI reaches these before B5. Identifiers, not paths, so an alias or
/// glob still has to name the item somewhere.
#[test]
fn split_b0_journal_api_has_no_gui_callers() {
    const ITEMS: [&str; 27] = [
        "create_split",
        "revalidate_split_construction",
        "bind_recovered_split_transaction",
        "record_split_broadcast_intent",
        "forget_split_descriptors",
        "abandon_split",
        "recorded_split",
        "split_identity",
        "RecordedSplit",
        // B0b: coordinator and transport.
        "SplitProduction",
        "resume_split",
        "submit_verified_split_step1_to_connect",
        "for_split_step1",
        // B3b: step-2 reservation and records.
        "record_split_target",
        "replace_used_split_target",
        "prepare_split_step2",
        "record_split_step2_broadcast_intent",
        "recorded_split_step2",
        // P3-3: step-2 resends and the observation that ends them.
        "record_split_step2_resubmission",
        "record_split_step2_observed",
        "split_step2_resubmissions",
        "split_step2_observed",
        "record_split_step2_returned",
        "split_step2_returned",
        "hold_split_step2_return",
        "release_split_step2_return",
        "Step2ReturnHold",
    ];
    const OWN: [&str; 4] = [
        "src/services/claim_workflow/split.rs",
        "src/services/claim_workflow/split/tests.rs",
        "src/services/claim_coordinator/split.rs",
        "src/services/claim_coordinator/split/tests.rs",
    ];
    fn walk(dir: &std::path::Path, files: &mut Vec<(String, String)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, files);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push((
                    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                    fs::read_to_string(&path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    let mut unexpected = Vec::new();
    for (file, text) in &files {
        let mut start = None;
        for (index, c) in text
            .char_indices()
            .chain(std::iter::once((text.len(), ' ')))
        {
            let word = c.is_ascii_alphanumeric() || c == '_';
            match (word, start) {
                (true, None) => start = Some(index),
                (false, Some(from)) => {
                    let ident = &text[from..index];
                    let reexport = file == "src/services/claim_workflow/mod.rs"
                        && ["split_identity", "RecordedSplit", "Step2ReturnHold"].contains(&ident);
                    let dispatch = file == "src/services/claim_coordinator/step1.rs"
                        && ident == "for_split_step1";
                    // B1b: the Split step-1 panel and its tests. Its only
                    // production constructor resumes an existing journal
                    // (`split_panel_has_no_gui_entry_point`), so no GUI action
                    // reaches these before B5.
                    let panel = file.starts_with("src/app/state/vault/split/")
                        && [
                            "create_split",
                            "resume_split",
                            "SplitProduction",
                            "split_identity",
                            "recorded_split",
                            "abandon_split",
                            "revalidate_split_construction",
                            "bind_recovered_split_transaction",
                        ]
                        .contains(&ident);
                    // B2/B3b: the dormant step-2 gate and construction and
                    // their tests reopen a submitted Split journal; no GUI
                    // caller reaches them
                    // (`fork::split::tests::split_step2_gate_has_no_gui_caller`).
                    // Only the tests create a journal (#626 guard nit).
                    let gate_tests =
                        file.starts_with("src/services/claim_coordinator/fork/split/tests");
                    let gate = file.starts_with("src/services/claim_coordinator/fork/split")
                        && ([
                            "SplitProduction",
                            "split_identity",
                            "recorded_split",
                            "revalidate_split_construction",
                            "bind_recovered_split_transaction",
                            "record_split_target",
                            "replace_used_split_target",
                            "prepare_split_step2",
                            "record_split_step2_broadcast_intent",
                            "recorded_split_step2",
                            "record_split_step2_resubmission",
                            "record_split_step2_observed",
                            "split_step2_resubmissions",
                            "split_step2_observed",
                            "record_split_step2_returned",
                            "split_step2_returned",
                            "hold_split_step2_return",
                            "release_split_step2_return",
                            "Step2ReturnHold",
                        ]
                        .contains(&ident)
                            || (gate_tests && ident == "create_split"));
                    // B3b-2b: the panel's (uncalled) step-2 layer reads the
                    // recorded step 2 at restart; its tests write journals.
                    // P3-3: the restart also reads whether a resend is
                    // allowed, to choose which handle to open (it records
                    // nothing); its tests record a return or a sighting.
                    let panel_step2 = (file.starts_with("src/app/state/vault/split/step2")
                        && [
                            "split_identity",
                            "recorded_split_step2",
                            "create_split",
                            "record_split_broadcast_intent",
                            "record_split_target",
                            "prepare_split_step2",
                            "record_split_step2_broadcast_intent",
                            "split_step2_returned",
                            "split_step2_observed",
                            "split_step2_resubmissions",
                        ]
                        .contains(&ident))
                        || (file.starts_with("src/app/state/vault/split/step2/tests")
                            && ["record_split_step2_returned", "record_split_step2_observed"]
                                .contains(&ident));
                    if ITEMS.contains(&ident)
                        && !OWN.contains(&file.as_str())
                        && !reexport
                        && !dispatch
                        && !panel
                        && !panel_step2
                        && !gate
                    {
                        unexpected.push((file.clone(), ident.to_owned()));
                    }
                    start = None;
                }
                _ => {}
            }
        }
    }
    assert!(unexpected.is_empty(), "{:?}", unexpected);
}

/// A P2WSH script of an unrelated wallet: a stand-in for the target Vault's
/// receive address at `index`.
fn target_script(index: u32) -> coincube_core::miniscript::bitcoin::ScriptBuf {
    make_wallet(Shape::WshMulti, 20)
        .source
        .external()
        .at_derivation_index(index)
        .unwrap()
        .script_pubkey()
}
/// The core step 2 of `wallet`'s claimed coins into `target`.
fn step2(
    wallet: &Wallet,
    step1: &SplitStep1,
    target: &coincube_core::miniscript::bitcoin::Script,
    feerate: u64,
) -> SplitStep2 {
    let coins = [
        coin(&wallet.source, SplitBranch::External, 0, 150_000),
        coin(&wallet.source, SplitBranch::Internal, 1, 70_000),
    ];
    coincube_core::foreign_split::create_split_step2(
        &coincube_core::foreign_split::SplitStep2Inputs {
            chain: ChainId::BitcoinBlake2b,
            source: &wallet.source,
            coins: &coins,
            fork_height: FORK,
            claimed: &step1.claimed_prevouts(),
            target,
        },
        feerate,
        LockTime::from_height(100).unwrap(),
        100,
    )
    .unwrap()
}
fn sign_step2(wallet: &Wallet, construction: &SplitStep2) -> VerifiedSplitStep2 {
    let secp = Secp256k1::new();
    let mut psbt = construction.psbt().clone();
    for signer in wallet.signers.iter().take(2) {
        psbt.sign(signer, &secp).unwrap();
    }
    let coins = [
        coin(&wallet.source, SplitBranch::External, 0, 150_000),
        coin(&wallet.source, SplitBranch::Internal, 1, 70_000),
    ];
    coincube_core::foreign_split::finalize_split_step2(
        construction,
        &coins,
        &wallet.source,
        &psbt,
        &secp,
    )
    .unwrap()
}

/// B3b: the step-2 target is reserved only once step 1 is tracked, kept
/// (same reservation again is a no-op, any other a conflict) and replaced
/// only by a strictly higher index; the unsigned step 2 must spend exactly
/// the claimed prevouts into it; the submission records the signed bytes
/// and names *their* txid (P2SH-P2WPKH scriptSigs change it). Once a step 2
/// is recorded, the target is fixed.
#[test]
fn split_step2_target_and_submission_records() {
    let (wallet, step1, signed) = setup(Shape::ShWpkh);
    let temp = Temp::new();
    let mut c = create(&temp, &step1, &signed);
    // Not before step 1 is tracked.
    assert!(matches!(
        c.record_split_target(&context(), 3, target_script(3)),
        Err(Error::InvalidPlan)
    ));
    record(&mut c, &signed);
    assert!(matches!(
        c.record_split_target(&context(), 3, target_script(3)),
        Err(Error::InvalidPlan)
    ));
    let tracked = signed.transaction().compute_txid();
    assert_eq!(
        refresh(
            &mut c,
            observation(tracked, Bitcoin::Confirmed { depth: 6 })
        ),
        Status::Observation(Assessment::ObservationsEligibleForPreflight)
    );
    c.record_split_target(&context(), 3, target_script(3))
        .unwrap();
    c.record_split_target(&context(), 3, target_script(3))
        .unwrap();
    for (index, script) in [(3, target_script(4)), (4, target_script(4))] {
        assert!(matches!(
            c.record_split_target(&context(), index, script),
            Err(Error::Conflict)
        ));
    }
    // Not a P2WSH/P2TR script.
    assert!(c
        .replace_used_split_target(
            &context(),
            3,
            4,
            signed.transaction().output[0].script_pubkey.clone()
        )
        .is_err());
    for (used, index) in [(2, 5), (3, 3), (3, 2)] {
        assert!(matches!(
            c.replace_used_split_target(&context(), used, index, target_script(index)),
            Err(Error::Conflict)
        ));
    }
    c.replace_used_split_target(&context(), 3, 5, target_script(5))
        .unwrap();
    let recorded = c.recorded_split().unwrap().unwrap();
    assert_eq!(recorded.target_index, Some(5));
    assert_eq!(recorded.target_script, Some(target_script(5)));

    // The unsigned step 2 needs a fresh assessment and the reserved target.
    let elsewhere = step2(&wallet, &step1, &target_script(6), 2);
    let construction = step2(&wallet, &step1, &target_script(5), 2);
    refresh(
        &mut c,
        observation(tracked, Bitcoin::Confirmed { depth: 6 }),
    );
    assert!(matches!(
        c.prepare_split_step2(&context(), &elsewhere, policy(), 10_000),
        Err(Error::WrongIdentity)
    ));
    // That attempt consumed the fresh assessment.
    assert!(matches!(
        c.prepare_split_step2(&context(), &construction, policy(), 10_000),
        Err(Error::Unchecked)
    ));
    refresh(
        &mut c,
        observation(tracked, Bitcoin::Confirmed { depth: 6 }),
    );
    c.prepare_split_step2(&context(), &construction, policy(), 10_000)
        .unwrap();
    assert_eq!(
        c.recorded_fork_sweep(),
        Some(&construction.psbt().unsigned_tx)
    );
    // Another construction (another fee) is refused; the same is a no-op.
    refresh(
        &mut c,
        observation(tracked, Bitcoin::Confirmed { depth: 6 }),
    );
    assert!(matches!(
        c.prepare_split_step2(
            &context(),
            &step2(&wallet, &step1, &target_script(5), 3),
            policy(),
            10_000
        ),
        Err(Error::Conflict)
    ));
    refresh(
        &mut c,
        observation(tracked, Bitcoin::Confirmed { depth: 6 }),
    );
    c.prepare_split_step2(&context(), &construction, policy(), 10_000)
        .unwrap();
    // The target is now fixed.
    assert!(matches!(
        c.replace_used_split_target(&context(), 5, 7, target_script(7)),
        Err(Error::Conflict)
    ));

    // The submission intent names the signed bytes' own txid.
    let verified = sign_step2(&wallet, &construction);
    let signed2 = verified.transaction().clone();
    assert_ne!(signed2.compute_txid(), construction.txid(), "sh(wpkh)");
    assert!(matches!(
        c.record_split_step2_broadcast_intent(&context(), &verified, policy(), 10_000),
        Err(Error::Unchecked)
    ));
    refresh(
        &mut c,
        observation(tracked, Bitcoin::Confirmed { depth: 6 }),
    );
    c.record_split_step2_broadcast_intent(&context(), &verified, policy(), 10_000)
        .unwrap();
    let submission = c.recorded_fork_submission().unwrap();
    assert_eq!(submission.txid(), signed2.compute_txid());
    assert_eq!(submission.wtxid(), signed2.compute_wtxid());
    assert_eq!(c.recorded_split_step2(), Some(&signed2));
    // Never recorded twice.
    refresh(
        &mut c,
        observation(tracked, Bitcoin::Confirmed { depth: 6 }),
    );
    assert!(matches!(
        c.record_split_step2_broadcast_intent(&context(), &verified, policy(), 10_000),
        Err(Error::Conflict)
    ));
    let base = c.intent.clone();
    drop(c);
    let c = reopen(&temp, &step1).unwrap();
    assert_eq!(c.recorded_split_step2(), Some(&signed2));

    // Validation of what a tampered journal could claim.
    full_validate(&base).unwrap();
    let check = |change: &dyn Fn(&mut Intent)| {
        let mut next = base.clone();
        change(&mut next);
        full_validate(&next).err()
    };
    let unsigned_txid = construction.txid();
    for (name, change) in [
        (
            "submission naming the unsigned txid",
            Box::new(move |i: &mut Intent| {
                i.fork_submission = Some(RecordedForkSubmission {
                    txid: unsigned_txid,
                    wtxid: i.fork_submission.unwrap().wtxid,
                })
            }) as Box<dyn Fn(&mut Intent)>,
        ),
        (
            "signed step 2 without a submission",
            Box::new(|i: &mut Intent| i.fork_submission = None),
        ),
        (
            "submission without the signed step 2",
            Box::new(|i: &mut Intent| i.split.as_mut().unwrap().step2_transaction = None),
        ),
        (
            "signed step 2 of another sweep",
            Box::new(|i: &mut Intent| {
                let sweep = i.fork_sweep.as_mut().unwrap();
                sweep.output[0].value = Amount::from_sat(sweep.output[0].value.to_sat() - 1);
            }),
        ),
        (
            "an unsigned step 2 recorded as signed",
            Box::new(|i: &mut Intent| {
                let record = i.split.as_mut().unwrap();
                let tx = record.step2_transaction.as_mut().unwrap();
                tx.input[0].script_sig = Default::default();
                tx.input[0].witness.clear();
            }),
        ),
        (
            "a sweep to another target",
            Box::new(|i: &mut Intent| {
                i.split.as_mut().unwrap().target_script = Some(target_script(9))
            }),
        ),
    ] {
        assert!(check(&*change).is_some(), "{}", name);
    }
}

/// A Split journal through a recorded step-2 submission of `wallet`'s
/// claimed coins into `target_script(5)`.
fn submitted() -> (
    Temp,
    Controller,
    Wallet,
    SplitStep1,
    VerifiedSplitStep2,
    Txid,
) {
    let (wallet, step1, signed) = setup(Shape::ShWpkh);
    let temp = Temp::new();
    let mut c = create(&temp, &step1, &signed);
    record(&mut c, &signed);
    let tracked = signed.transaction().compute_txid();
    refresh(
        &mut c,
        observation(tracked, Bitcoin::Confirmed { depth: 6 }),
    );
    c.record_split_target(&context(), 5, target_script(5))
        .unwrap();
    let construction = step2(&wallet, &step1, &target_script(5), 2);
    refresh(
        &mut c,
        observation(tracked, Bitcoin::Confirmed { depth: 6 }),
    );
    c.prepare_split_step2(&context(), &construction, policy(), 10_000)
        .unwrap();
    let verified = sign_step2(&wallet, &construction);
    refresh(
        &mut c,
        observation(tracked, Bitcoin::Confirmed { depth: 6 }),
    );
    c.record_split_step2_broadcast_intent(&context(), &verified, policy(), 10_000)
        .unwrap();
    (temp, c, wallet, step1, verified, tracked)
}

/// P3-3: a resend of the recorded signed step 2 is recorded only after its
/// latest attempt's return without acceptance was recorded (which the
/// resend clears), after a fresh assessment whose read of it on BTCB2 was
/// absent, for exactly its bytes, never once it was seen there, and within
/// the attempt bound. A
/// sighting is recorded once, from a read of its own txid. Neither field
/// exists until used, so an earlier journal serializes as before, and each
/// needs a recorded step 2.
#[test]
fn split_step2_resends_and_the_observation_that_ends_them() {
    let (temp, mut c, wallet, step1, verified, tracked) = submitted();
    let txid = verified.transaction().compute_txid();
    let text = journal_text(&temp);
    assert!(!["step2_resubmissions", "step2_observed", "step2_returned"]
        .iter()
        .any(|field| text.contains(field)));
    assert_eq!(c.split_step2_resubmissions(), 0);
    assert!(!c.split_step2_observed() && !c.split_step2_returned());
    let fresh = |c: &mut Controller| {
        refresh(c, observation(tracked, Bitcoin::Confirmed { depth: 6 }));
    };
    // A resend takes the permission withdrawn for its read: the latest
    // attempt's recorded return, given back to it if a refusal consumed it.
    let held = |c: &mut Controller| {
        if !c.split_step2_returned() {
            c.record_split_step2_returned(&context()).unwrap();
        }
        c.hold_split_step2_return(&context()).unwrap().unwrap()
    };
    let resend = |c: &mut Controller, signed: &VerifiedSplitStep2, step2| {
        let hold = held(c);
        c.record_split_step2_resubmission(&context(), signed, step2, hold, policy(), 10_000)
    };
    // Nothing to withdraw before the submission's return is recorded, so no
    // resend can be recorded.
    assert!(c.hold_split_step2_return(&context()).unwrap().is_none());
    c.record_split_step2_returned(&context()).unwrap();
    c.record_split_step2_returned(&context()).unwrap();
    assert!(c.split_step2_returned());
    let text = journal_text(&temp);
    // Withdrawn for a read, then given back: the journal is as it was.
    let hold = c.hold_split_step2_return(&context()).unwrap().unwrap();
    assert!(!c.split_step2_returned());
    assert!(c.hold_split_step2_return(&context()).unwrap().is_none());
    c.release_split_step2_return(&context(), hold).unwrap();
    assert_eq!(journal_text(&temp), text);
    // A fresh assessment, an absent step 2 and the recorded bytes. Every
    // refusal consumes the withdrawn permission: it stays withdrawn.
    assert!(matches!(
        resend(&mut c, &verified, TransactionObservation::Absent),
        Err(Error::Unchecked)
    ));
    assert!(!c.split_step2_returned());
    fresh(&mut c);
    assert!(matches!(
        resend(
            &mut c,
            &verified,
            TransactionObservation::Unconfirmed { txid }
        ),
        Err(Error::Unchecked)
    ));
    let other = sign_step2(&wallet, &step2(&wallet, &step1, &target_script(5), 3));
    fresh(&mut c);
    assert!(matches!(
        resend(&mut c, &other, TransactionObservation::Absent),
        Err(Error::InvalidPlan)
    ));
    // Not eligible (step 1 five deep).
    refresh(
        &mut c,
        observation(tracked, Bitcoin::Confirmed { depth: 5 }),
    );
    assert!(matches!(
        resend(&mut c, &verified, TransactionObservation::Absent),
        Err(Error::Unchecked)
    ));
    assert!(!c.split_step2_returned());
    fresh(&mut c);
    resend(&mut c, &verified, TransactionObservation::Absent).unwrap();
    assert_eq!(c.split_step2_resubmissions(), 1);
    // It consumed the assessment and the permission; its own return is not
    // recorded yet.
    assert!(!c.split_step2_returned());
    assert!(c.hold_split_step2_return(&context()).unwrap().is_none());
    fresh(&mut c);
    resend(&mut c, &verified, TransactionObservation::Absent).unwrap();
    let journal: serde_json::Value = serde_json::from_str(&journal_text(&temp)).unwrap();
    let wtxid = verified.transaction().compute_wtxid().to_string();
    assert_eq!(
        journal["split"]["step2_resubmissions"],
        serde_json::json!([{ "wtxid": wtxid }, { "wtxid": wtxid }])
    );
    // Nothing recorded earlier changes.
    assert_eq!(c.recorded_split_step2(), Some(verified.transaction()));
    assert_eq!(c.recorded_fork_submission().unwrap().txid(), txid);
    // A permission withdrawn for a read that then records a sighting is not
    // given back.
    c.record_split_step2_returned(&context()).unwrap();
    let hold = c.hold_split_step2_return(&context()).unwrap().unwrap();
    let before_sighting = c.intent.clone();
    c.record_split_step2_observed(&context(), TransactionObservation::Unconfirmed { txid })
        .unwrap();
    assert!(matches!(
        c.release_split_step2_return(&context(), hold),
        Err(Error::Conflict)
    ));
    assert!(!c.split_step2_returned());
    // Undo the sighting in memory only, to check the remaining rules on
    // their own.
    c.intent = before_sighting;
    c.journal.store(&c.intent.clone()).unwrap();

    // A sighting: only of the recorded txid; an absence changes nothing.
    c.record_split_step2_observed(&context(), TransactionObservation::Absent)
        .unwrap();
    assert!(!c.split_step2_observed());
    let elsewhere = Txid::from_byte_array([9; 32]);
    assert!(matches!(
        c.record_split_step2_observed(
            &context(),
            TransactionObservation::Unconfirmed { txid: elsewhere }
        ),
        Err(Error::InvalidPlan)
    ));
    c.record_split_step2_observed(&context(), TransactionObservation::Unconfirmed { txid })
        .unwrap();
    c.record_split_step2_observed(&context(), TransactionObservation::Unconfirmed { txid })
        .unwrap();
    assert!(c.split_step2_observed());
    fresh(&mut c);
    assert!(matches!(
        resend(&mut c, &verified, TransactionObservation::Absent),
        Err(Error::Conflict)
    ));
    let base = c.intent.clone();
    drop(c);
    let c = reopen(&temp, &step1).unwrap();
    assert_eq!(c.split_step2_resubmissions(), 2);
    assert!(c.split_step2_observed());
    drop(c);

    // At the bound, refused.
    let (_temp, mut c, _, _, verified, _) = submitted();
    let attempt = Step2Resubmission {
        wtxid: verified.transaction().compute_wtxid(),
    };
    let at_bound = c.intent.split.as_mut().unwrap();
    at_bound.step2_resubmissions = vec![attempt; MAX_SPLIT_STEP2_RESUBMISSIONS];
    at_bound.step2_returned = true;
    full_validate(&c.intent).unwrap();
    c.journal.store(&c.intent.clone()).unwrap();
    fresh(&mut c);
    assert!(matches!(
        resend(&mut c, &verified, TransactionObservation::Absent),
        Err(Error::Conflict)
    ));

    // What a tampered journal could claim.
    full_validate(&base).unwrap();
    let check = |change: &dyn Fn(&mut Intent)| {
        let mut next = base.clone();
        change(&mut next);
        full_validate(&next).err()
    };
    for (name, change) in [
        (
            "a resend of other bytes",
            Box::new(|i: &mut Intent| {
                i.split.as_mut().unwrap().step2_resubmissions[0].wtxid =
                    coincube_core::miniscript::bitcoin::Wtxid::from_byte_array([7; 32])
            }) as Box<dyn Fn(&mut Intent)>,
        ),
        (
            "past the bound",
            Box::new(move |i: &mut Intent| {
                i.split.as_mut().unwrap().step2_resubmissions =
                    vec![attempt; MAX_SPLIT_STEP2_RESUBMISSIONS + 1]
            }),
        ),
        (
            "resends without a recorded step 2",
            Box::new(|i: &mut Intent| {
                i.fork_submission = None;
                let record = i.split.as_mut().unwrap();
                record.step2_transaction = None;
                record.step2_observed = false;
            }),
        ),
        (
            "an observation without a recorded step 2",
            Box::new(|i: &mut Intent| {
                i.fork_submission = None;
                let record = i.split.as_mut().unwrap();
                record.step2_transaction = None;
                record.step2_resubmissions.clear();
            }),
        ),
        (
            "a return without a recorded step 2",
            Box::new(|i: &mut Intent| {
                i.fork_submission = None;
                let record = i.split.as_mut().unwrap();
                record.step2_transaction = None;
                record.step2_resubmissions.clear();
                record.step2_observed = false;
                record.step2_returned = true;
            }),
        ),
    ] {
        assert!(check(&*change).is_some(), "{}", name);
    }
    // Without a recorded step 2, a sighting has nothing to name.
    let (_wallet, step1, signed) = setup(Shape::ShWpkh);
    let temp = Temp::new();
    let mut c = create(&temp, &step1, &signed);
    record(&mut c, &signed);
    assert!(matches!(
        c.record_split_step2_observed(
            &context(),
            TransactionObservation::Unconfirmed {
                txid: signed.transaction().compute_txid()
            }
        ),
        Err(Error::InvalidPlan)
    ));
    assert!(matches!(
        c.record_split_step2_returned(&context()),
        Err(Error::InvalidPlan)
    ));
    assert_eq!(c.split_step2_resubmissions(), 0);
}
