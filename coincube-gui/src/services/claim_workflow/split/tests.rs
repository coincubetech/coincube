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

/// The BIP39 seed of `seed`'s mnemonic (entropy `[seed; 16]`), so that the
/// same key can sign `ALL|UNIFIED` through a session signer (B4b-3a).
fn mnemonic(seed: u8) -> coincube_core::bip39::Mnemonic {
    coincube_core::bip39::Mnemonic::from_entropy(&[seed; 16]).unwrap()
}
fn master(seed: u8) -> Xpriv {
    // PBKDF2 per call is slow in a debug build; each seed is derived once.
    static MASTERS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u8, Xpriv>>> =
        std::sync::OnceLock::new();
    *MASTERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(seed)
        .or_insert_with(|| {
            Xpriv::new_master(Network::Bitcoin, &mnemonic(seed).to_seed("")).unwrap()
        })
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
    /// The seed byte of each signer, in order.
    seeds: Vec<u8>,
}
fn make_wallet(shape: Shape, seed: u8) -> Wallet {
    let (template, seeds) = match shape {
        Shape::Wpkh => (
            format!("wpkh({}/{{b}}/*)", account(&master(seed), "m/84'/0'/0'")),
            vec![seed],
        ),
        Shape::ShWpkh => (
            format!(
                "sh(wpkh({}/{{b}}/*))",
                account(&master(seed), "m/49'/0'/0'")
            ),
            vec![seed],
        ),
        Shape::Pkh => (
            format!("pkh({}/{{b}}/*)", account(&master(seed), "m/44'/0'/0'")),
            vec![seed],
        ),
        Shape::WshMulti => {
            let seeds = vec![seed, seed + 1, seed + 2];
            let keys: Vec<_> = seeds
                .iter()
                .map(|s| format!("{}/{{b}}/*", account(&master(*s), "m/48'/0'/0'/2'")))
                .collect();
            (format!("wsh(multi(2,{}))", keys.join(",")), seeds)
        }
    };
    let branch = |b: u32| Descriptor::from_str(&template.replace("{b}", &b.to_string())).unwrap();
    Wallet {
        source: SplitSource::new(branch(0), Some(branch(1))).unwrap(),
        signers: seeds.iter().map(|s| master(*s)).collect(),
        seeds,
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
    const ITEMS: [&str; 38] = [
        "create_split",
        // B4b-1b: the fork-only (`kind: Unified`) record.
        "create_unified_split",
        "revalidate_unified_construction",
        "record_unified_broadcast_intent",
        "SplitKind",
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
        // #625 F2: the step-2 dead end a split may be closed in.
        "split_step2_dead_end",
        // #568 S4: the step-1 conflict (O4) after the step-2 submission.
        "Step1Conflict",
        "split_step1_conflict",
        "record_split_step1_conflict",
        "confirm_split_step1_conflict",
        "clear_split_step1_conflict",
        "disprove_split_step1_conflict",
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
                        && [
                            "split_identity",
                            "RecordedSplit",
                            "Step2ReturnHold",
                            "SplitKind",
                            "Step1Conflict",
                        ]
                        .contains(&ident);
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
                    // Only the tests create a journal (#626 guard nit); B4b-1b's
                    // reconciler tests create and submit a fork-only one.
                    let gate_tests =
                        file.starts_with("src/services/claim_coordinator/fork/split/tests");
                    let gate = file.starts_with("src/services/claim_coordinator/fork/split")
                        && ([
                            "SplitProduction",
                            "split_identity",
                            "recorded_split",
                            "revalidate_split_construction",
                            "bind_recovered_split_transaction",
                            // B5a: the completion evidence deletes them.
                            "forget_split_descriptors",
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
                            // #568 S4: the step-2 reconciler's step-1 reorg
                            // outcomes refuse a fork-only record and record
                            // a step-1 conflict (O4).
                            "SplitKind",
                            "Step1Conflict",
                            "split_step1_conflict",
                            "record_split_step1_conflict",
                            "confirm_split_step1_conflict",
                            "clear_split_step1_conflict",
                            "disprove_split_step1_conflict",
                        ]
                        .contains(&ident)
                            || (gate_tests
                                && [
                                    "create_split",
                                    "create_unified_split",
                                    "record_unified_broadcast_intent",
                                ]
                                .contains(&ident)));
                    // B4b-3a: the fork-only route's coordinator (dormant,
                    // guarded with the step-2 gate) is the one production
                    // writer of a fork-only record: it creates the journal at
                    // confirmation, revalidates a reopened one and records
                    // the verified sweep's intent.
                    let unified = file
                        == "src/services/claim_coordinator/fork/split/step2/unified.rs"
                        && [
                            "create_unified_split",
                            "revalidate_unified_construction",
                            "record_unified_broadcast_intent",
                        ]
                        .contains(&ident);
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
                            // #625 F2: the restart reads the dead end.
                            "split_step2_dead_end",
                        ]
                        .contains(&ident))
                        || (file.starts_with("src/app/state/vault/split/step2/tests")
                            && [
                                "record_split_step2_returned",
                                "record_split_step2_observed",
                                // #568 S4: the warning test builds an O4 outcome.
                                "Step1Conflict",
                            ]
                            .contains(&ident));
                    // #625 F2: the close tests make a journal whose resend is
                    // reviewable, which is no dead end. Tests only.
                    let panel_step2_tests = file
                        .starts_with("src/app/state/vault/split/step2/tests")
                        && ident == "record_split_step2_returned";
                    if ITEMS.contains(&ident)
                        && !OWN.contains(&file.as_str())
                        && !reexport
                        && !dispatch
                        && !panel
                        && !panel_step2
                        && !panel_step2_tests
                        && !gate
                        && !unified
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

/// #625 F2: a new split is never created in a directory holding a closed
/// split's tombstone, whatever it is (a partly reset directory must not hide
/// a new journal behind an old tombstone); without it, creation proceeds.
#[test]
fn split_create_refuses_a_directory_with_a_tombstone() {
    let (_wallet, step1, signed) = setup(Shape::ShWpkh);
    let create_split = |temp: &Temp| {
        Controller::create_split(&temp.0, TARGET.into(), &step1, &signed, FORK, context())
    };
    let temp = Temp::new();
    fs::write(temp.0.join(SPLIT_TOMBSTONE), b"{}").unwrap();
    assert!(matches!(create_split(&temp), Err(Error::Conflict)));
    assert!(!temp.0.join("intent.json").exists());
    let temp = Temp::new();
    fs::create_dir(temp.0.join(SPLIT_TOMBSTONE)).unwrap();
    assert!(matches!(create_split(&temp), Err(Error::Conflict)));
    assert!(!temp.0.join("intent.json").exists());
    let temp = Temp::new();
    create_split(&temp).unwrap();
}

/// #625 F2: a step-2 dead end is a recorded submission that no resend can
/// follow (its permission withdrawn, or the resend limit reached) and no
/// read ever saw on BTCB2. Before any submission, with a resend reviewable,
/// or once step 2 was seen, it is not one.
#[test]
fn split_step2_dead_end_is_a_submission_no_resend_or_sighting_can_follow() {
    let (_wallet, step1, signed) = setup(Shape::ShWpkh);
    let temp = Temp::new();
    let mut c = create(&temp, &step1, &signed);
    assert!(!c.split_step2_dead_end());
    record(&mut c, &signed);
    assert!(!c.split_step2_dead_end());

    let (_temp, mut c, _wallet, _step1, verified, tracked) = submitted();
    // Accepted, cancelled, timed out or interrupted: no return recorded.
    assert!(c.split_step2_dead_end());
    // A completed send that came back unaccepted: a resend is reviewable.
    c.record_split_step2_returned(&context()).unwrap();
    assert!(!c.split_step2_dead_end());
    // Withdrawn for a read that never gave it back: a dead end again.
    let _hold = c.hold_split_step2_return(&context()).unwrap().unwrap();
    assert!(c.split_step2_dead_end());
    // Every resend the journal allows, the last one's return recorded: the
    // permission stands, but no resend can follow.
    for _ in 0..MAX_SPLIT_STEP2_RESUBMISSIONS {
        c.record_split_step2_returned(&context()).unwrap();
        let hold = c.hold_split_step2_return(&context()).unwrap().unwrap();
        refresh(
            &mut c,
            observation(tracked, Bitcoin::Confirmed { depth: 6 }),
        );
        c.record_split_step2_resubmission(
            &context(),
            &verified,
            TransactionObservation::Absent,
            hold,
            policy(),
            10_000,
        )
        .unwrap();
    }
    c.record_split_step2_returned(&context()).unwrap();
    assert!(c.split_step2_returned());
    assert_eq!(c.split_step2_resubmissions(), MAX_SPLIT_STEP2_RESUBMISSIONS);
    assert!(c.split_step2_dead_end());
    // Seen on BTCB2: it left, so it is not a dead end.
    let txid = verified.transaction().compute_txid();
    c.record_split_step2_observed(&context(), TransactionObservation::Unconfirmed { txid })
        .unwrap();
    assert!(!c.split_step2_dead_end());
}

// ---------------------------------------------------------------------------
// B4b-1b: the fork-only (`kind: Unified`) record.

/// Core's unified sweep (B4b-1a) of `wallet`'s two splittable coins into
/// `target` on `chain` against `fork_height`: one output, no change.
fn unified_sweep_on(
    wallet: &Wallet,
    target: &coincube_core::miniscript::bitcoin::Script,
    feerate: u64,
    chain: ChainId,
    fork_height: u64,
) -> UnifiedSweep {
    let coins = [
        coin(&wallet.source, SplitBranch::External, 0, 150_000),
        coin(&wallet.source, SplitBranch::Internal, 1, 70_000),
    ];
    coincube_core::foreign_split::create_unified_sweep(
        &coincube_core::foreign_split::UnifiedInputs {
            chain,
            source: &wallet.source,
            coins: &coins,
            fork_height,
            target,
        },
        feerate,
        LockTime::from_height(100).unwrap(),
        100,
    )
    .unwrap()
}
fn unified_sweep(
    wallet: &Wallet,
    target: &coincube_core::miniscript::bitcoin::Script,
    feerate: u64,
) -> UnifiedSweep {
    unified_sweep_on(wallet, target, feerate, ChainId::BitcoinBlake2b, FORK)
}
/// `sweep` signed `ALL|UNIFIED` by the wallet's seeds (two for multisig, one
/// otherwise) and verified by core's unified finalizer: Protected.
fn sign_unified(wallet: &Wallet, sweep: &UnifiedSweep) -> VerifiedUnifiedSweep {
    let secp = Secp256k1::new();
    let mut psbt =
        coincube_core::psbt_unified::UnifiedPsbt::from_psbt(sweep.psbt().clone()).unwrap();
    for seed in wallet.seeds.iter().take(2) {
        let signer = coincube_core::signer::SessionSigner::from_mnemonic(
            Network::Bitcoin,
            mnemonic(*seed),
            "",
        )
        .unwrap();
        psbt = signer.sign_unified(&psbt, sweep.chain(), &secp).unwrap();
    }
    coincube_core::foreign_split::finalize_unified_sweep(sweep, &psbt, &secp).unwrap()
}
fn create_unified(temp: &Temp, sweep: &UnifiedSweep, context: Context) -> Controller {
    Controller::create_unified_split(&temp.0, TARGET.into(), sweep, 5, context).unwrap()
}
fn unified_identity(wallet: &Wallet) -> WalletIdentity {
    split_identity(TARGET.into(), wallet.source.digest())
}
fn reopen_unified(temp: &Temp, wallet: &Wallet, context: Context) -> Result<Controller, Error> {
    Controller::reopen_settling_blocking(&temp.0, &unified_identity(wallet), context)
}

/// B4b-1b: a fork-only record is a version-9, owner-only journal with
/// `kind: Unified`, no step 1 (the canonical empty transaction, nothing
/// tracked, signed or attempted on Bitcoin, no `bitcoin_transaction`),
/// Tracking from creation, its target and unsigned sweep recorded from
/// creation, and the signed sweep recorded with its submission, which names
/// the signed bytes' own txid. It round-trips through reopen under the Split
/// identity. A two-step record, written and rewritten by this binary, still
/// serializes as the version-8 shape without a `kind` field, which the
/// version-8 reader (mirrored here) reads; that reader refuses the fork-only
/// record on its unknown `kind` field and, had it known the field, on its
/// version.
#[test]
fn fork_only_journal_round_trips_and_old_readers_refuse() {
    let wallet = make_wallet(Shape::ShWpkh, 1);
    let target = target_script(5);
    let sweep = unified_sweep(&wallet, &target, 2);
    let unsigned = sweep.psbt().unsigned_tx.clone();
    let temp = Temp::new();
    let mut c = create_unified(&temp, &sweep, context());
    #[cfg(unix)]
    assert_eq!(mode(&temp), 0o600);
    let json: serde_json::Value = serde_json::from_str(&journal_text(&temp)).unwrap();
    assert_eq!(json["version"], 9);
    assert_eq!(json["split"]["kind"], "Unified");
    assert_eq!(json["identity"]["bitcoin_cube"], "");
    assert_eq!(json["identity"]["fork_cube"], TARGET);
    assert_eq!(json["split"]["target_cube"], TARGET);
    assert_eq!(json["split"]["fork_height"], FORK);
    assert_eq!(json["split"]["destination"], 0);
    assert_eq!(json["split"]["target_index"], 5);
    assert!(json["split"].get("target_script").is_some());
    assert!(json["split"].get("step2_transaction").is_none());
    assert_eq!(
        json["split"]["descriptors"]["external"],
        wallet.source.external().to_string()
    );
    assert_eq!(json["phase"], "Tracking");
    assert!(json["signed_txid"].is_null());
    assert!(json.get("bitcoin_transaction").is_none());
    assert!(json.get("bitcoin_attempts").is_none());
    assert!(json.get("fork_submission").is_none());
    assert!(json["plan"].get("tracked_txid").is_none());
    assert!(json["plan"].get("previous_confirmation").is_some());
    assert!(json["plan"]["previous_confirmation"].is_null());
    assert!(json["plan"]["step1"]["input"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(json["plan"]["step1"]["output"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(json.get("fork_sweep").is_some());
    assert_eq!(c.phase(), Phase::Tracking);
    assert_eq!(c.signed_txid(), None);
    assert_eq!(c.recorded_bitcoin_transaction(), None);
    assert_eq!(c.last_inclusion(), None);
    assert_eq!(c.recorded_fork_sweep(), Some(&unsigned));
    assert_eq!(c.recorded_fork_submission(), None);
    assert_eq!(c.recorded_split_step2(), None);
    assert_eq!(c.status(), Status::Unchecked);
    assert_eq!(
        c.plan().claimed_prevouts,
        unsigned
            .input
            .iter()
            .map(|i| i.previous_output)
            .collect::<Vec<_>>()
    );
    let recorded = c.recorded_split().unwrap().unwrap();
    assert_eq!(recorded.kind, SplitKind::Unified);
    assert_eq!(recorded.source.as_ref(), Some(&wallet.source));
    assert_eq!(recorded.source_digest, wallet.source.digest());
    assert_eq!((recorded.fork_height, recorded.destination), (FORK, 0));
    assert_eq!(recorded.target_index, Some(5));
    assert_eq!(recorded.target_script.as_deref(), Some(target.as_script()));
    assert_eq!(c.identity(), &unified_identity(&wallet));
    // The open controller holds the journal's lock.
    assert!(matches!(
        Controller::create_unified_split(&temp.0, TARGET.into(), &sweep, 5, context()),
        Err(Error::Busy)
    ));
    let (_, step1, signed1) = setup(Shape::Pkh);

    // The submission records the signed sweep and names its own txid
    // (sh(wpkh) scriptSigs change it).
    let verified = sign_unified(&wallet, &sweep);
    let signed = verified.transaction().clone();
    assert_ne!(signed.compute_txid(), unsigned.compute_txid());
    c.record_unified_broadcast_intent(&context(), &verified)
        .unwrap();
    let submission = c.recorded_fork_submission().unwrap();
    assert_eq!(submission.txid(), signed.compute_txid());
    assert_eq!(submission.wtxid(), signed.compute_wtxid());
    assert_eq!(c.recorded_split_step2(), Some(&signed));
    assert_eq!(c.phase(), Phase::Tracking);
    drop(c);
    let c = reopen_unified(&temp, &wallet, context()).unwrap();
    assert_eq!(
        c.recorded_split().unwrap().unwrap().kind,
        SplitKind::Unified
    );
    assert_eq!(c.recorded_split_step2(), Some(&signed));
    assert_eq!(c.recorded_fork_sweep(), Some(&unsigned));
    assert_eq!(c.recorded_fork_submission(), Some(submission));
    assert_eq!(c.status(), Status::Unchecked);
    let json: serde_json::Value = serde_json::from_str(&journal_text(&temp)).unwrap();
    assert_eq!(json["version"], 9);
    assert_eq!(json["split"]["kind"], "Unified");
    assert!(json["split"].get("step2_transaction").is_some());
    drop(c);
    // One intent per directory, whatever the kind.
    assert!(matches!(
        Controller::create_unified_split(&temp.0, TARGET.into(), &sweep, 5, context()),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        Controller::create_split(&temp.0, TARGET.into(), &step1, &signed1, FORK, context()),
        Err(Error::Conflict)
    ));
    // A Claim identity, or another target, never opens it.
    let mut wrong = unified_identity(&wallet);
    wrong.bitcoin_cube = "btc-cube".into();
    assert!(matches!(
        Controller::reopen_settling_blocking(&temp.0, &wrong, context()),
        Err(Error::WrongIdentity)
    ));
    let mut wrong = unified_identity(&wallet);
    wrong.fork_cube = "another-target".into();
    assert!(matches!(
        Controller::reopen_settling_blocking(&temp.0, &wrong, context()),
        Err(Error::WrongIdentity)
    ));
    let text = journal_text(&temp);

    // The version-8 reader: the Split record's fields as of version 8, with
    // no `kind`. It reads a two-step journal, which still carries no `kind`
    // and is version 8, and refuses the fork-only one on that field; its
    // validator also refused every version but 8.
    #[allow(dead_code)]
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct V8Split {
        source_digest: serde_json::Value,
        #[serde(default)]
        descriptors: Option<serde_json::Value>,
        fork_height: u64,
        destination: u32,
        target_cube: String,
        #[serde(default)]
        target_index: Option<u32>,
        #[serde(default)]
        target_script: Option<serde_json::Value>,
        #[serde(default)]
        step2_transaction: Option<serde_json::Value>,
        #[serde(default)]
        step2_resubmissions: Option<serde_json::Value>,
        #[serde(default)]
        step2_observed: Option<bool>,
        #[serde(default)]
        step2_returned: Option<bool>,
    }
    #[allow(dead_code)]
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct V8Intent {
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
        #[serde(default)]
        split: Option<V8Split>,
    }
    let error = serde_json::from_str::<V8Intent>(&text).err().unwrap();
    assert!(
        error.to_string().contains("unknown field `kind`"),
        "{}",
        error
    );
    assert_ne!(
        json["version"], 8,
        "the version-8 validator refuses any other version"
    );

    // A two-step journal, written and rewritten by this binary, is still the
    // version-8 shape that reader reads: no `kind` anywhere, version 8.
    let two_step = Temp::new();
    let mut c = create(&two_step, &step1, &signed1);
    record(&mut c, &signed1);
    refresh(
        &mut c,
        observation(
            signed1.transaction().compute_txid(),
            Bitcoin::Confirmed { depth: 6 },
        ),
    );
    c.record_split_target(&context(), 3, target_script(3))
        .unwrap();
    drop(c);
    let text = journal_text(&two_step);
    assert_eq!(serde_json::from_str::<V8Intent>(&text).unwrap().version, 8);
    assert!(!text.contains("kind"), "{}", text);
    let c = reopen(&two_step, &step1).unwrap();
    assert_eq!(c.recorded_split().unwrap().unwrap().kind, SplitKind::Split);
}

/// B4b-1b: `validate` refuses a fork-only record with any of the two-step
/// shape (a `bitcoin_transaction`, a step 1, a signed or tracked txid, a
/// Bitcoin confirmation or attempt, a destination, phase Intent), one
/// without its target or sweep, one at version 8 or claiming to be two-step;
/// and a two-step record in the fork-only shape, at version 9, or claiming
/// to be fork-only.
#[test]
fn unified_record_validate_refuses_split_shape_and_vice_versa() {
    let wallet = make_wallet(Shape::Wpkh, 1);
    let target = target_script(5);
    let sweep = unified_sweep(&wallet, &target, 2);
    let temp = Temp::new();
    let base = create_unified(&temp, &sweep, context()).intent.clone();
    full_validate(&base).unwrap();
    let (_, step1, signed1) = setup(Shape::Wpkh);
    let two_step = Temp::new();
    let split_base = create(&two_step, &step1, &signed1).intent.clone();
    full_validate(&split_base).unwrap();
    let check = |base: &Intent, change: &dyn Fn(&mut Intent)| {
        let mut next = base.clone();
        change(&mut next);
        full_validate(&next).err()
    };
    type Change = Box<dyn Fn(&mut Intent)>;
    let signed_step1 = signed1.transaction().clone();
    let step1_tx = step1.psbt().unsigned_tx.clone();
    let txid = signed_step1.compute_txid();
    let fork_only: Vec<(&str, Change)> = vec![
        (
            "with a bitcoin_transaction",
            Box::new(move |i| i.bitcoin_transaction = Some(signed_step1.clone())),
        ),
        (
            "with a step 1",
            Box::new(move |i| {
                i.plan.step1 = step1_tx.clone();
                i.unsigned_digest = digest(&i.plan.step1);
            }),
        ),
        (
            "with a step 1 that is empty but not the canonical empty transaction",
            Box::new(|i| {
                i.plan.step1.lock_time = LockTime::from_height(1).unwrap();
                i.unsigned_digest = digest(&i.plan.step1);
            }),
        ),
        (
            "without a target",
            Box::new(|i| {
                let record = i.split.as_mut().unwrap();
                record.target_index = None;
                record.target_script = None;
            }),
        ),
        ("at version 8", Box::new(|i| i.version = 8)),
        ("in phase Intent", Box::new(|i| i.phase = Phase::Intent)),
        (
            "in phase BroadcastUncertain",
            Box::new(|i| i.phase = Phase::BroadcastUncertain),
        ),
        (
            "with a signed txid",
            Box::new(move |i| i.signed_txid = Some(txid)),
        ),
        (
            "with a tracked txid",
            Box::new(move |i| i.plan.tracked_txid = Some(txid)),
        ),
        (
            "with a Bitcoin confirmation",
            Box::new(|i| {
                i.plan.previous_confirmation = Some(BlockRef {
                    height: 100,
                    hash: hash(4),
                })
            }),
        ),
        (
            "with a Bitcoin attempt",
            Box::new(|i| {
                i.bitcoin_attempts
                    .push(BitcoinSubmissionAttempt { wtxid: None })
            }),
        ),
        (
            "with a destination",
            Box::new(|i| i.split.as_mut().unwrap().destination = 5),
        ),
        ("without its sweep", Box::new(|i| i.fork_sweep = None)),
        (
            "whose sweep does not spend exactly the claimed prevouts",
            Box::new(|i| {
                i.plan.claimed_prevouts.pop();
            }),
        ),
        (
            "with no claimed prevouts",
            Box::new(|i| i.plan.claimed_prevouts.clear()),
        ),
        (
            "with a Bitcoin cube",
            Box::new(|i| i.identity.bitcoin_cube = "btc".into()),
        ),
        (
            "claiming to be two-step",
            Box::new(|i| i.split.as_mut().unwrap().kind = SplitKind::Split),
        ),
        (
            "claiming to be two-step at version 8",
            Box::new(|i| {
                i.version = 8;
                i.split.as_mut().unwrap().kind = SplitKind::Split;
            }),
        ),
    ];
    for (name, change) in &fork_only {
        assert!(
            check(&base, &**change).is_some(),
            "a fork-only record {}",
            name
        );
    }
    let two_step: Vec<(&str, Change)> = vec![
        (
            "in the fork-only shape",
            Box::new(|i| {
                i.plan.step1 = empty_step1();
                i.unsigned_digest = digest(&i.plan.step1);
                i.plan.tracked_txid = None;
                i.bitcoin_transaction = None;
                i.signed_txid = None;
                i.phase = Phase::Tracking;
            }),
        ),
        ("at version 9", Box::new(|i| i.version = 9)),
        (
            "claiming to be fork-only",
            Box::new(|i| i.split.as_mut().unwrap().kind = SplitKind::Unified),
        ),
        (
            "claiming to be fork-only at version 9",
            Box::new(|i| {
                i.version = 9;
                i.split.as_mut().unwrap().kind = SplitKind::Unified;
            }),
        ),
    ];
    for (name, change) in &two_step {
        assert!(
            check(&split_base, &**change).is_some(),
            "a two-step record {}",
            name
        );
    }
}

/// B4b-1b: a fork-only record whose submission is recorded keeps the
/// step-2 fields a reconcile reads: the identity, a journal that validates,
/// a recorded fork submission, a Split record, the Bitcoin chain, and a
/// recorded signed sweep whose txid is the submission's. (Since B4b-3a the
/// step-2 reconciler refuses the record and the fork-only reconciler
/// opens it, recording only the sighting; both are named only in
/// `fork::split`, by that module's D1 guard, so the tests that open them
/// live there: `fork::split::tests::step2::{unified_journal,
/// unified_flow}`.) The journal still takes, in order, the writes a step-2
/// reconcile would make: no return hold (no attempt
/// returned), a check whose step-1-centric assessment is inert (there is no
/// step 1 to assess), then the sighting of the recorded txid, which
/// survives reopen. It is never a step-2 dead end, even in the state that
/// is one for a two-step record: that close checks a step 1 the record does
/// not have, and its close is B4b-3's decision.
#[test]
fn unified_record_keeps_the_step2_fields_a_fork_only_reconcile_reads() {
    let ctx = context();
    let wallet = make_wallet(Shape::WshMulti, 1);
    let target = target_script(5);
    let sweep = unified_sweep(&wallet, &target, 2);
    let temp = Temp::new();
    drop(create_unified(&temp, &sweep, ctx.clone()));
    let mut c = reopen_unified(&temp, &wallet, ctx.clone()).unwrap();
    // Nothing to reconcile before a submission is recorded.
    assert_eq!(c.recorded_fork_submission(), None);
    assert_eq!(c.recorded_split_step2(), None);
    c.revalidate_unified_construction(&ctx, &sweep, 5).unwrap();
    let verified = sign_unified(&wallet, &sweep);
    let signed = verified.transaction().clone();
    c.record_unified_broadcast_intent(&ctx, &verified).unwrap();
    assert!(!c.split_step2_dead_end());
    drop(c);
    // What the reconciler's reopen reads.
    let c = Controller::reopen_settling_blocking(
        &temp.0,
        &split_identity(TARGET.into(), wallet.source.digest()),
        ctx.clone(),
    )
    .unwrap();
    let submission = c.recorded_fork_submission().unwrap();
    assert!(c.recorded_split().unwrap().is_some());
    assert_eq!(c.plan().bitcoin_chain, ChainId::Bitcoin);
    assert_eq!(
        c.recorded_split_step2().map(|tx| tx.compute_txid()),
        Some(submission.txid())
    );
    assert_eq!(submission.wtxid(), signed.compute_wtxid());
    drop(c);

    // The journal side of the reconcile, on the reopened record.
    let mut c = reopen_unified(&temp, &wallet, ctx.clone()).unwrap();
    assert!(c.hold_split_step2_return(&ctx).unwrap().is_none());
    let ticket = c.begin_check(&ctx).unwrap();
    let status = c
        .apply_observation(
            ticket,
            &ctx,
            Ok(observation(signed.compute_txid(), Bitcoin::Absent)),
            policy(),
            10_000,
        )
        .unwrap();
    assert_eq!(status, Status::Observation(Assessment::InvalidPlan));
    assert_eq!(c.phase(), Phase::Tracking);
    // The sighting must be of the recorded txid.
    assert!(matches!(
        c.record_split_step2_observed(
            &ctx,
            TransactionObservation::Unconfirmed {
                txid: Txid::from_byte_array([9; 32])
            }
        ),
        Err(Error::InvalidPlan)
    ));
    assert!(!c.split_step2_observed());
    c.record_split_step2_observed(
        &ctx,
        TransactionObservation::Unconfirmed {
            txid: signed.compute_txid(),
        },
    )
    .unwrap();
    assert!(c.split_step2_observed());
    assert!(!c.split_step2_dead_end());
    drop(c);
    let c = reopen_unified(&temp, &wallet, ctx).unwrap();
    assert!(c.split_step2_observed());
    assert_eq!(c.recorded_split_step2(), Some(&signed));
    // The same journal state of a two-step record is a dead end.
    let (_temp, two_step, ..) = submitted();
    assert!(two_step.split_step2_dead_end());
}

/// B4b-1b limitation, pinned: the step-2 observation path (`collect_sweep`,
/// which the step-2 reconciler's `reconcile_sweep` runs) refuses a fork-only
/// plan before any read, since it is step-1-centric (the claimed prevouts
/// must be the step 1's inputs and the step 1 must carry the poison). The
/// fork-only reconcile is B4b-3a's separate fork-only observation (U1),
/// which leaves this refusal in place. Lifting it is a deliberate change.
#[tokio::test]
async fn step2_observation_path_refuses_a_fork_only_plan() {
    use crate::services::claim_observation::{
        collect_sweep, CollectionContext, FailureKind, FreshRead, ObservationSource, Stage,
    };
    use crate::services::coincube::network_anchor::NetworkAnchorStatus;
    struct Unreachable;
    #[async_trait::async_trait]
    impl ObservationSource for Unreachable {
        fn now(&self) -> i64 {
            unreachable!("no read is made")
        }
        async fn anchor(&self, _: ChainId) -> Result<NetworkAnchorStatus, FailureKind> {
            unreachable!("no read is made")
        }
        async fn tip(&self, _: ChainId) -> Result<FreshRead<BlockRef>, FailureKind> {
            unreachable!("no read is made")
        }
        async fn transaction(
            &self,
            _: ChainId,
            _: Txid,
        ) -> Result<FreshRead<TransactionObservation>, FailureKind> {
            unreachable!("no read is made")
        }
        async fn hash_at_height(
            &self,
            _: ChainId,
            _: u64,
        ) -> Result<FreshRead<BlockHash>, FailureKind> {
            unreachable!("no read is made")
        }
    }
    let wallet = make_wallet(Shape::Wpkh, 1);
    let target = target_script(5);
    let sweep = unified_sweep(&wallet, &target, 2);
    let temp = Temp::new();
    let mut c = create_unified(&temp, &sweep, context());
    let verified = sign_unified(&wallet, &sweep);
    let signed = verified.transaction().clone();
    c.record_unified_broadcast_intent(&context(), &verified)
        .unwrap();
    let (_sender, generation) = tokio::sync::watch::channel(1u64);
    let failure = collect_sweep(
        &Unreachable,
        &c.plan(),
        signed.compute_txid(),
        policy(),
        std::time::Duration::from_secs(5),
        CollectionContext {
            expected_generation: 1,
            generation,
        },
    )
    .await
    .err()
    .unwrap();
    assert!(
        matches!(
            (failure.stage, failure.kind),
            (Stage::Plan, FailureKind::InvalidPlan)
        ),
        "{:?}",
        failure
    );
}

/// B4b-1b: a fork-only record's abandonment is B4b-3's decision. The
/// journal refuses it before and after a submission, and keeps the record;
/// nor is the record ever a step-2 dead end to close.
#[test]
fn unified_record_abandon_is_refused() {
    let wallet = make_wallet(Shape::Pkh, 1);
    let target = target_script(5);
    let sweep = unified_sweep(&wallet, &target, 2);
    let temp = Temp::new();
    let c = create_unified(&temp, &sweep, context());
    // Fresh, nothing sent: still refused, and kept.
    assert!(matches!(c.abandon_split(&context()), Err(Error::Conflict)));
    assert!(temp.0.join("intent.json").exists());
    let c = reopen_unified(&temp, &wallet, context()).unwrap();
    assert!(matches!(c.abandon_split(&context()), Err(Error::Conflict)));
    let mut c = reopen_unified(&temp, &wallet, context()).unwrap();
    c.revalidate_unified_construction(&context(), &sweep, 5)
        .unwrap();
    let verified = sign_unified(&wallet, &sweep);
    c.record_unified_broadcast_intent(&context(), &verified)
        .unwrap();
    assert!(!c.split_step2_dead_end());
    assert!(matches!(c.abandon_split(&context()), Err(Error::Conflict)));
    let c = reopen_unified(&temp, &wallet, context()).unwrap();
    assert_eq!(c.recorded_split_step2(), Some(verified.transaction()));
    assert!(!c.split_step2_dead_end());
    // A revoked session refuses too, as for a two-step record.
    let mut other = context();
    other.generation = 2;
    assert!(matches!(c.abandon_split(&other), Err(Error::Revoked)));
    assert!(temp.0.join("intent.json").exists());
}

/// B4b-1b: a fork-only record refuses every two-step writer (the step-1
/// revalidation, binding and submission, the descriptor deletion, the
/// reservation it already holds and its replacement, the step-2 record,
/// submission and resend), and a two-step record refuses the fork-only
/// writers at every phase. Neither refusal disturbs the record.
#[test]
fn unified_and_two_step_writers_refuse_each_others_record() {
    let wallet = make_wallet(Shape::ShWpkh, 1);
    let target = target_script(5);
    let sweep = unified_sweep(&wallet, &target, 2);
    let temp = Temp::new();
    let mut c = create_unified(&temp, &sweep, context());
    let (other_wallet, step1, signed1) = setup(Shape::ShWpkh);
    assert!(matches!(
        c.revalidate_split_construction(&context(), &step1, FORK),
        Err(Error::WrongIdentity)
    ));
    assert!(matches!(
        c.bind_recovered_split_transaction(&context(), &signed1),
        Err(Error::WrongIdentity)
    ));
    assert!(matches!(
        c.record_split_broadcast_intent(&context(), &signed1, policy(), 10_000),
        Err(Error::WrongIdentity)
    ));
    assert!(matches!(
        c.forget_split_descriptors(&context()),
        Err(Error::WrongIdentity)
    ));
    // Even the reservation it already holds: the target is fixed at creation.
    assert!(matches!(
        c.record_split_target(&context(), 5, target.clone()),
        Err(Error::WrongIdentity)
    ));
    assert!(matches!(
        c.replace_used_split_target(&context(), 5, 6, target_script(6)),
        Err(Error::WrongIdentity)
    ));
    let step2_construction = step2(&other_wallet, &step1, &target, 2);
    assert!(matches!(
        c.prepare_split_step2(&context(), &step2_construction, policy(), 10_000),
        Err(Error::WrongIdentity)
    ));
    let verified2 = sign_step2(&other_wallet, &step2_construction);
    assert!(matches!(
        c.record_split_step2_broadcast_intent(&context(), &verified2, policy(), 10_000),
        Err(Error::WrongIdentity)
    ));
    assert!(matches!(
        c.record_split_step2_resubmission(
            &context(),
            &verified2,
            TransactionObservation::Absent,
            Step2ReturnHold {
                controller: 0,
                resubmissions: 0,
            },
            policy(),
            10_000
        ),
        Err(Error::WrongIdentity)
    ));
    // Undisturbed: still verified from creation, target and descriptors kept.
    let recorded = c.recorded_split().unwrap().unwrap();
    assert_eq!(recorded.target_index, Some(5));
    assert_eq!(recorded.source.as_ref(), Some(&wallet.source));
    c.record_unified_broadcast_intent(&context(), &sign_unified(&wallet, &sweep))
        .unwrap();

    // The reverse, at every phase of a two-step record.
    let two_step = Temp::new();
    let mut c = create(&two_step, &step1, &signed1);
    let other_sweep = unified_sweep(&other_wallet, &target, 2);
    let other_signed = sign_unified(&other_wallet, &other_sweep);
    let tracked = signed1.transaction().compute_txid();
    for phase in [Phase::Intent, Phase::BroadcastUncertain, Phase::Tracking] {
        assert_eq!(c.phase(), phase);
        assert!(matches!(
            c.revalidate_unified_construction(&context(), &other_sweep, 5),
            Err(Error::WrongIdentity)
        ));
        assert!(matches!(
            c.record_unified_broadcast_intent(&context(), &other_signed),
            Err(Error::WrongIdentity)
        ));
        match phase {
            Phase::Intent => record(&mut c, &signed1),
            Phase::BroadcastUncertain => {
                refresh(
                    &mut c,
                    observation(tracked, Bitcoin::Confirmed { depth: 6 }),
                );
            }
            Phase::Tracking => {}
        }
    }
    // Undisturbed: the two-step flow continues (verified from creation).
    assert_eq!(c.recorded_fork_submission(), None);
    c.record_split_target(&context(), 5, target.clone())
        .unwrap();
}

/// B4b-1b, B4b-3a (Reviewer-650 F2): creation takes core's unified sweep
/// itself, so a sweep of another shape (a second output, a target it does
/// not pay, the Bitcoin chain, no fork height, a target that is neither
/// P2WSH nor P2TR) cannot be described to it at all; a target index outside
/// the normal range is refused and writes nothing. After a restart the
/// construction is unverified until the exact rebuild is checked; a
/// different sweep, chain, source, fork height, target index or target
/// refuses and leaves it unverified. The submission takes only core's
/// verified unified sweep, of exactly the recorded construction on the
/// record's fork chain, once, in the same session.
#[test]
fn unified_submission_needs_a_revalidated_exact_construction() {
    let wallet = make_wallet(Shape::WshMulti, 1);
    let target = target_script(5);
    let sweep = unified_sweep(&wallet, &target, 2);
    let temp = Temp::new();
    assert!(matches!(
        Controller::create_unified_split(&temp.0, TARGET.into(), &sweep, 1 << 31, context()),
        Err(Error::InvalidPlan)
    ));
    assert!(!temp.0.join("intent.json").exists());
    drop(create_unified(&temp, &sweep, context()));

    let mut c = reopen_unified(&temp, &wallet, context()).unwrap();
    let verified = sign_unified(&wallet, &sweep);
    let signed = verified.transaction().clone();
    assert!(matches!(
        c.record_unified_broadcast_intent(&context(), &verified),
        Err(Error::Unchecked)
    ));
    let other_sweep = unified_sweep(&wallet, &target, 3);
    let other_wallet = make_wallet(Shape::WshMulti, 7);
    let other_source = unified_sweep(&other_wallet, &target, 2);
    let elsewhere = target_script(6);
    let other_target = unified_sweep(&wallet, &elsewhere, 2);
    let other_chain = unified_sweep_on(&wallet, &target, 2, ChainId::BitcoinBlake2bTestnet4, FORK);
    let other_height = unified_sweep_on(&wallet, &target, 2, ChainId::BitcoinBlake2b, FORK + 1);
    assert_eq!(
        other_height.psbt().unsigned_tx,
        sweep.psbt().unsigned_tx,
        "the fork height is not in the transaction"
    );
    for (name, wrong, index) in [
        ("another sweep", &other_sweep, 5),
        ("another chain", &other_chain, 5),
        ("another source", &other_source, 5),
        ("another fork height", &other_height, 5),
        ("another target index", &sweep, 6),
        ("another target script", &other_target, 5),
    ] {
        assert!(
            matches!(
                c.revalidate_unified_construction(&context(), wrong, index),
                Err(Error::WrongIdentity)
            ),
            "{}",
            name
        );
        assert!(
            matches!(
                c.record_unified_broadcast_intent(&context(), &verified),
                Err(Error::Unchecked)
            ),
            "{}",
            name
        );
    }
    c.revalidate_unified_construction(&context(), &sweep, 5)
        .unwrap();
    // Exactly the recorded construction, on the record's fork chain.
    for (name, wrong) in [
        ("another sweep", sign_unified(&wallet, &other_sweep)),
        ("another chain", sign_unified(&wallet, &other_chain)),
    ] {
        assert!(
            matches!(
                c.record_unified_broadcast_intent(&context(), &wrong),
                Err(Error::InvalidPlan)
            ),
            "{}",
            name
        );
        assert_eq!(c.recorded_fork_submission(), None, "{}", name);
    }
    // A revoked session refuses.
    let mut other = context();
    other.generation = 2;
    assert!(matches!(
        c.record_unified_broadcast_intent(&other, &verified),
        Err(Error::Revoked)
    ));
    drop(c);
    let mut c = reopen_unified(&temp, &wallet, context()).unwrap();
    c.revalidate_unified_construction(&context(), &sweep, 5)
        .unwrap();
    c.record_unified_broadcast_intent(&context(), &verified)
        .unwrap();
    // Never twice.
    assert!(matches!(
        c.record_unified_broadcast_intent(&context(), &verified),
        Err(Error::Conflict)
    ));
    assert_eq!(c.recorded_split_step2(), Some(&signed));
}

/// B4b-3a (Reviewer-650 F2): what the journal records as the submitted sweep
/// is a sweep core verified Protected: rebuilt from the journal, its recorded
/// bytes verify as `ALL|UNIFIED` on every input. The same sweep with legacy
/// `ALL` signatures, which the raw-transaction writer of #650 journaled,
/// fails that verification; the typed writer cannot be handed one.
#[test]
fn unified_journal_records_only_a_protected_sweep() {
    use coincube_core::foreign_split::{verify_unified_sweep_transaction, UnifiedReplayStatus};
    for shape in [Shape::Pkh, Shape::ShWpkh, Shape::Wpkh, Shape::WshMulti] {
        let wallet = make_wallet(shape, 1);
        let target = target_script(5);
        let sweep = unified_sweep(&wallet, &target, 2);
        let temp = Temp::new();
        let mut c = create_unified(&temp, &sweep, context());
        let verified = sign_unified(&wallet, &sweep);
        c.record_unified_broadcast_intent(&context(), &verified)
            .unwrap();
        drop(c);
        let c = reopen_unified(&temp, &wallet, context()).unwrap();
        let recorded = c.recorded_split_step2().unwrap().clone();
        let secp = Secp256k1::verification_only();
        let reverified = verify_unified_sweep_transaction(&sweep, &recorded, &secp).unwrap();
        assert_eq!(
            reverified.replay_status(),
            UnifiedReplayStatus::Protected,
            "{:?}",
            shape
        );
        // The bytes #650's raw writer accepted: every input signed, ALL.
        let mut legacy = sweep.psbt().clone();
        for signer in wallet.signers.iter().take(2) {
            legacy.sign(signer, &Secp256k1::new()).unwrap();
        }
        let legacy = coincube_core::foreign_split::finalize_split_step2(
            &coincube_core::foreign_split::create_split_step2(
                &coincube_core::foreign_split::SplitStep2Inputs {
                    chain: ChainId::BitcoinBlake2b,
                    source: &wallet.source,
                    coins: &[
                        coin(&wallet.source, SplitBranch::External, 0, 150_000),
                        coin(&wallet.source, SplitBranch::Internal, 1, 70_000),
                    ],
                    fork_height: FORK,
                    claimed: &sweep.spent_outpoints(),
                    target: &target,
                },
                2,
                LockTime::from_height(100).unwrap(),
                100,
            )
            .unwrap(),
            &[
                coin(&wallet.source, SplitBranch::External, 0, 150_000),
                coin(&wallet.source, SplitBranch::Internal, 1, 70_000),
            ],
            &wallet.source,
            &legacy,
            &secp,
        )
        .unwrap()
        .transaction()
        .clone();
        assert_eq!(
            unsigned(&legacy),
            sweep.psbt().unsigned_tx,
            "{:?}: the same sweep",
            shape
        );
        assert!(
            verify_unified_sweep_transaction(&sweep, &legacy, &secp).is_err(),
            "{:?}",
            shape
        );
    }
}

/// #654 I2 (Reviewer-654d M2, M3, M7): the workflow-level checks that keep
/// a step-1 conflict (O4) off a fork-only record are each pinned on their
/// own: the conflict writers refuse the kind before anything else
/// (`WrongIdentity`, not a later validation), and a fork-only journal edited
/// to carry a conflict is refused when it is read. Each mutation that drops
/// one of these kind checks fails here even while the others hold.
#[test]
fn fork_only_record_never_carries_a_step1_conflict() {
    let wallet = make_wallet(Shape::ShWpkh, 1);
    let target = target_script(5);
    let sweep = unified_sweep(&wallet, &target, 2);
    let temp = Temp::new();
    let mut c = create_unified(&temp, &sweep, context());
    c.record_unified_broadcast_intent(&context(), &sign_unified(&wallet, &sweep))
        .unwrap();
    let outpoint = c.plan().claimed_prevouts[0];
    let tip = coincube_core::claim::BlockRef {
        height: 1_000,
        hash: coincube_core::miniscript::bitcoin::BlockHash::from_byte_array([4; 32]),
    };
    let conflict = Step1Conflict::new(outpoint, tip);
    // M7: the writer refuses the kind itself.
    assert!(matches!(
        c.record_split_step1_conflict(&context(), conflict),
        Err(Error::WrongIdentity)
    ));
    // M3: so does the disproof, which would otherwise read "nothing to
    // clear" and succeed.
    assert!(matches!(
        c.disprove_split_step1_conflict(&context()),
        Err(Error::WrongIdentity)
    ));
    assert_eq!(c.split_step1_conflict(), None);
    drop(c);
    // M2: a fork-only journal edited to carry the conflict is refused. The
    // same journal rewritten without the edit reads, so the refusal is the
    // conflict's.
    let mut json: serde_json::Value = serde_json::from_str(&journal_text(&temp)).unwrap();
    fs::write(
        temp.0.join("intent.json"),
        serde_json::to_vec_pretty(&json).unwrap(),
    )
    .unwrap();
    drop(reopen_unified(&temp, &wallet, context()).unwrap());
    json["split"]["step1_conflict"] = serde_json::to_value(conflict).unwrap();
    fs::write(
        temp.0.join("intent.json"),
        serde_json::to_vec_pretty(&json).unwrap(),
    )
    .unwrap();
    assert!(reopen_unified(&temp, &wallet, context()).is_err());
}
