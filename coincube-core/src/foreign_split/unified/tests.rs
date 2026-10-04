//! The fixtures are a copy of step 2's (that file stays unchanged), with
//! each wallet's keys derived from a BIP39 session signer so that the
//! unified path (`SessionSigner::sign_unified`) and rust-bitcoin's legacy
//! reference signer (`Psbt::sign`, for the P1 refusals) hold the same keys.

use super::*;
use crate::signer::SessionSigner;
use miniscript::bitcoin::{
    bip32::{DerivationPath, Xpriv},
    Network,
};
use std::{convert::TryFrom, str::FromStr};

const FORK: u64 = 900;
const FEERATE: u64 = 5;
/// Observed BTCB2 tip height for the sweep's locktime.
const BTCB2_TIP: u32 = 861_000;
const BTCB2: ChainId = ChainId::BitcoinBlake2b;

type Change = Box<dyn Fn(&mut Psbt)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Wpkh,
    ShWpkh,
    Pkh,
    WshSortedMulti,
    WshMulti,
}
const SHAPES: [Shape; 5] = [
    Shape::Wpkh,
    Shape::ShWpkh,
    Shape::Pkh,
    Shape::WshSortedMulti,
    Shape::WshMulti,
];

/// One seed: the session signer the unified path signs with, and the same
/// master key for rust-bitcoin's legacy signer.
struct Seed {
    session: SessionSigner,
    master: Xpriv,
}

fn seed(byte: u8) -> Seed {
    let mnemonic = bip39::Mnemonic::from_entropy(&[byte; 16]).unwrap();
    let master = Xpriv::new_master(Network::Bitcoin, &mnemonic.to_seed("")).unwrap();
    let session = SessionSigner::from_mnemonic(Network::Bitcoin, mnemonic, "").unwrap();
    Seed { session, master }
}

fn account(seed: &Seed, path: &str) -> String {
    let secp = secp256k1::Secp256k1::new();
    let origin = DerivationPath::from_str(path).unwrap();
    format!(
        "[{}/{}]{}",
        seed.session.fingerprint(&secp),
        path.trim_start_matches("m/"),
        seed.session.xpub_at(&origin, &secp)
    )
}

fn descriptor(text: &str) -> Descriptor<DescriptorPublicKey> {
    Descriptor::from_str(text).unwrap()
}

struct Wallet {
    source: SplitSource,
    seeds: Vec<Seed>,
    threshold: usize,
}

fn wallet(shape: Shape) -> Wallet {
    let branch =
        |template: &str, branch: u32| descriptor(&template.replace("{b}", &branch.to_string()));
    let (template, seeds, threshold) = match shape {
        Shape::Wpkh => {
            let s = seed(1);
            (
                format!("wpkh({}/{{b}}/*)", account(&s, "m/84'/0'/0'")),
                vec![s],
                1,
            )
        }
        Shape::ShWpkh => {
            let s = seed(1);
            (
                format!("sh(wpkh({}/{{b}}/*))", account(&s, "m/49'/0'/0'")),
                vec![s],
                1,
            )
        }
        Shape::Pkh => {
            let s = seed(1);
            (
                format!("pkh({}/{{b}}/*)", account(&s, "m/44'/0'/0'")),
                vec![s],
                1,
            )
        }
        Shape::WshSortedMulti | Shape::WshMulti => {
            let seeds = vec![seed(1), seed(2), seed(3)];
            let keys: Vec<_> = seeds
                .iter()
                .map(|s| format!("{}/{{b}}/*", account(s, "m/48'/0'/0'/2'")))
                .collect();
            let name = if shape == Shape::WshMulti {
                "multi"
            } else {
                "sortedmulti"
            };
            // Two of the three seeds sign.
            (format!("wsh({name}(2,{}))", keys.join(",")), seeds, 2)
        }
    };
    Wallet {
        source: SplitSource::new(branch(&template, 0), Some(branch(&template, 1))).unwrap(),
        seeds,
        threshold,
    }
}

fn block(height: u64) -> BlockRef {
    BlockRef {
        height,
        hash: BlockHash::from_byte_array([height as u8; 32]),
    }
}

fn coin(source: &SplitSource, branch: SplitBranch, index: u32, sats: u64) -> SplitCoin {
    let script = source.derive(branch, index).unwrap().script_pubkey();
    let previous = Transaction {
        version: transaction::Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::new(Txid::from_byte_array([index as u8 + 1; 32]), index),
            ..TxIn::default()
        }],
        output: vec![
            TxOut {
                value: Amount::from_sat(1_000),
                script_pubkey: ScriptBuf::new_op_return(
                    bitcoin::script::PushBytesBuf::try_from(vec![index as u8]).unwrap(),
                ),
            },
            TxOut {
                value: Amount::from_sat(sats),
                script_pubkey: script,
            },
        ],
    };
    SplitCoin {
        outpoint: OutPoint::new(previous.compute_txid(), 1),
        branch,
        index,
        previous,
        bitcoin_block: Some(block(FORK - 10)),
        btcb2_block: Some(block(FORK - 10)),
    }
}

fn coins(source: &SplitSource) -> Vec<SplitCoin> {
    vec![
        coin(source, SplitBranch::External, 0, 100_000),
        coin(source, SplitBranch::Internal, 3, 50_000),
    ]
}

/// A target Cube's Vault address: a P2WSH 2-of-3 of unrelated keys.
fn vault_target(seeds: std::ops::RangeInclusive<u8>) -> ScriptBuf {
    let keys: Vec<_> = seeds
        .map(|byte| format!("{}/0/*", account(&seed(byte), "m/48'/0'/0'/2'")))
        .collect();
    descriptor(&format!("wsh(sortedmulti(2,{}))", keys.join(",")))
        .at_derivation_index(0)
        .unwrap()
        .script_pubkey()
}

fn inputs<'a>(
    source: &'a SplitSource,
    coins: &'a [SplitCoin],
    target: &'a bitcoin::Script,
) -> UnifiedInputs<'a> {
    UnifiedInputs {
        chain: BTCB2,
        source,
        coins,
        fork_height: FORK,
        target,
    }
}

fn create(inputs: &UnifiedInputs<'_>) -> Result<UnifiedSweep, Error> {
    create_unified_sweep(inputs, FEERATE, LockTime::ZERO, BTCB2_TIP)
}

fn unified(psbt: &Psbt) -> UnifiedPsbt {
    UnifiedPsbt::from_psbt(psbt.clone()).unwrap()
}

/// The production signing path: `SessionSigner::sign_unified`, one seed
/// after another, on Bitcoin Blake2b.
fn sign_unified(psbt: &Psbt, seeds: &[&Seed]) -> UnifiedPsbt {
    let secp = secp256k1::Secp256k1::new();
    let mut signed = unified(psbt);
    for seed in seeds {
        signed = seed.session.sign_unified(&signed, BTCB2, &secp).unwrap();
    }
    signed
}

/// rust-bitcoin's reference signer: legacy `SIGHASH_ALL` `partial_sigs`.
fn sign_legacy(psbt: &Psbt, seeds: &[&Seed]) -> Psbt {
    let secp = secp256k1::Secp256k1::new();
    let mut signed = psbt.clone();
    for seed in seeds {
        signed.sign(&seed.master, &secp).unwrap();
    }
    signed
}

/// A legacy-finalized transaction of the construction: what a standard
/// Bitcoin PSBT consumer would produce from the same keys.
fn legacy_transaction(psbt: &Psbt, seeds: &[&Seed]) -> Transaction {
    let secp = secp256k1::Secp256k1::new();
    let mut signed = sign_legacy(psbt, seeds);
    signed.finalize_mut(&secp).unwrap();
    signed.extract(&secp).unwrap()
}

fn finalize(
    sweep: &UnifiedSweep,
    signed: &UnifiedPsbt,
) -> Result<VerifiedUnifiedSweep, UnifiedSweepFinalizeError> {
    finalize_unified_sweep(sweep, signed, &secp256k1::Secp256k1::verification_only())
}

fn verify(
    sweep: &UnifiedSweep,
    transaction: &Transaction,
) -> Result<VerifiedUnifiedSweep, UnifiedSweepFinalizeError> {
    verify_unified_sweep_transaction(
        sweep,
        transaction,
        &secp256k1::Secp256k1::verification_only(),
    )
}

fn unsigned(tx: &Transaction) -> Transaction {
    let mut tx = tx.clone();
    for input in &mut tx.input {
        input.script_sig = ScriptBuf::new();
        input.witness.clear();
    }
    tx
}

/// Every scriptSig push and witness item of one input that parses as a DER
/// signature followed by a sighash byte.
fn signature_items(tx: &Transaction, index: usize) -> Vec<Vec<u8>> {
    let input = &tx.input[index];
    let pushes = input.script_sig.instructions().filter_map(|i| match i {
        Ok(Instruction::PushBytes(bytes)) => Some(bytes.as_bytes().to_vec()),
        _ => None,
    });
    pushes
        .chain(input.witness.iter().map(<[u8]>::to_vec))
        .filter(|item| {
            item.split_last()
                .is_some_and(|(_, der)| secp256k1::ecdsa::Signature::from_der(der).is_ok())
        })
        .collect()
}

/// One wallet, its coins and the Vault target.
struct Fixture {
    wallet: Wallet,
    coins: Vec<SplitCoin>,
    target: ScriptBuf,
}

impl Fixture {
    fn new(shape: Shape) -> Self {
        let wallet = wallet(shape);
        let coins = coins(&wallet.source);
        Fixture {
            wallet,
            coins,
            target: vault_target(5..=7),
        }
    }
    fn inputs(&self) -> UnifiedInputs<'_> {
        inputs(&self.wallet.source, &self.coins, &self.target)
    }
    fn sweep(&self) -> UnifiedSweep {
        create(&self.inputs()).unwrap()
    }
    /// The first `threshold` seeds: the quorum that signs.
    fn signers(&self) -> Vec<&Seed> {
        self.wallet.seeds[..self.wallet.threshold].iter().collect()
    }
}

/// Every shape builds step 2's construction (exactly the coins by outpoint,
/// one target output, no change), signs `ALL|UNIFIED` through the session
/// signer (two of three seeds for the multisigs), finalizes with every input
/// Protected, retains only `0x21` signatures in the witness, and verifies
/// again from the retained bytes alone.
#[test]
fn unified_sweep_every_shape_finalizes_protected() {
    for shape in SHAPES {
        let fixture = Fixture::new(shape);
        let sweep = fixture.sweep();
        let mut expected: Vec<_> = fixture.coins.iter().map(|c| c.outpoint).collect();
        expected.sort();
        assert_eq!(sweep.spent_outpoints(), expected, "{shape:?}");
        assert_eq!(sweep.psbt().unsigned_tx.output.len(), 1);
        assert_eq!(sweep.target(), fixture.target.as_script());
        assert_eq!(sweep.chain(), BTCB2);
        assert_eq!(sweep.fork_height(), FORK);
        assert_eq!(
            sweep.fee(),
            Amount::from_sat(sweep.maximum_signed_vbytes() * FEERATE)
        );
        assert_eq!(create(&fixture.inputs()).unwrap().psbt(), sweep.psbt());

        let signed = sign_unified(sweep.psbt(), &fixture.signers());
        for input in &signed.psbt().inputs {
            assert_eq!(input.sighash_type.map(|s| s.to_u32()), Some(0x21));
            assert!(input.partial_sigs.is_empty());
        }
        let verified = finalize(&sweep, &signed).unwrap_or_else(|e| panic!("{:?}: {}", shape, e));
        assert_eq!(verified.replay_status(), UnifiedReplayStatus::Protected);
        assert_eq!(verified.inputs().len(), 2);
        assert!(verified
            .inputs()
            .iter()
            .all(|r| r.replay_protected() && r.legacy_used == 0));
        assert!(verified
            .inputs()
            .iter()
            .all(|r| r.unified_used == fixture.wallet.threshold));
        assert_eq!(verified.chain(), BTCB2);
        assert_eq!(verified.construction_txid(), sweep.txid());
        assert_eq!(verified.fee(), sweep.fee());
        assert!(verified.vsize() as u64 <= sweep.maximum_signed_vbytes());
        let tx = verified.transaction();
        assert_eq!(unsigned(tx), sweep.psbt().unsigned_tx);
        for index in 0..tx.input.len() {
            let items = signature_items(tx, index);
            assert_eq!(
                items.len(),
                fixture.wallet.threshold,
                "{shape:?} input {index}"
            );
            assert!(items.iter().all(|item| item.last() == Some(&0x21)));
        }

        let again = verify(&sweep, tx).unwrap_or_else(|e| panic!("{:?}: {}", shape, e));
        assert_eq!(again.transaction(), tx);
        assert_eq!(again.replay_status(), UnifiedReplayStatus::Protected);
        assert_eq!(again.inputs(), verified.inputs());
        assert_eq!(again.fee(), verified.fee());
        assert_eq!(again.construction_txid(), sweep.txid());
    }
}

/// P1: a legacy signature anywhere is refused as `LegacySignature`, never
/// finalized or classified. Covered at signing (the unified signer refuses a
/// PSBT carrying `partial_sigs`), at finalization (a legacy-only PSBT and a
/// mixed one with unified records on every input plus one legacy entry),
/// and from retained bytes (a legacy-finalized transaction and a spliced one
/// with a legacy witness on one input).
#[test]
fn unified_sweep_refuses_legacy_and_mixed_signatures() {
    use UnifiedSweepFinalizeError::LegacySignature;
    for shape in SHAPES {
        let fixture = Fixture::new(shape);
        let sweep = fixture.sweep();
        let signers = fixture.signers();

        let legacy = sign_legacy(sweep.psbt(), &signers);
        assert!(!legacy.inputs[0].partial_sigs.is_empty());
        assert!(matches!(
            signers[0]
                .session
                .sign_unified(&unified(&legacy), BTCB2, &secp256k1::Secp256k1::new()),
            Err(ForeignUnifiedError::LegacySignature { input: 0 })
        ));
        assert_eq!(
            finalize(&sweep, &unified(&legacy)).err(),
            Some(LegacySignature { input: 0 }),
            "{shape:?}"
        );

        let mut mixed = sign_unified(sweep.psbt(), &signers);
        let one_legacy = sign_legacy(sweep.psbt(), &signers[..1]).inputs[1]
            .partial_sigs
            .clone();
        assert_eq!(one_legacy.len(), 1);
        mixed.psbt_mut().inputs[1].partial_sigs = one_legacy;
        assert_eq!(
            finalize(&sweep, &mixed).err(),
            Some(LegacySignature { input: 1 }),
            "{shape:?}"
        );

        let legacy_tx = legacy_transaction(sweep.psbt(), &signers);
        assert_eq!(unsigned(&legacy_tx), sweep.psbt().unsigned_tx);
        assert_eq!(
            verify(&sweep, &legacy_tx).err(),
            Some(LegacySignature { input: 0 }),
            "{shape:?}"
        );
        let unified_tx = finalize(&sweep, &sign_unified(sweep.psbt(), &signers))
            .unwrap()
            .transaction()
            .clone();
        let mut spliced = unified_tx.clone();
        spliced.input[1].script_sig = legacy_tx.input[1].script_sig.clone();
        spliced.input[1].witness = legacy_tx.input[1].witness.clone();
        assert_eq!(
            verify(&sweep, &spliced).err(),
            Some(LegacySignature { input: 1 }),
            "{shape:?}"
        );
    }
}

/// Construction refuses every chain but Bitcoin Blake2b; the signer refuses
/// to produce `0x21` on Bitcoin; and a `SIGHASH_ALL` request (or any request
/// other than `0x21`, or none) is refused at signing and at finalization,
/// however the signatures got there.
#[test]
fn unified_sweep_refuses_bitcoin_chain_and_all_request() {
    use UnifiedSweepFinalizeError::UnsupportedSighash;
    let fixture = Fixture::new(Shape::Wpkh);
    for chain in [
        ChainId::Bitcoin,
        ChainId::Testnet,
        ChainId::Testnet4,
        ChainId::Signet,
        ChainId::Regtest,
    ] {
        let mut inputs = fixture.inputs();
        inputs.chain = chain;
        assert_eq!(
            create(&inputs).err(),
            Some(Error::NotBitcoinBlake2b(chain)),
            "{chain:?}"
        );
        let recorded = fixture.sweep().psbt().unsigned_tx.clone();
        assert_eq!(
            reconstruct_unified_sweep(&inputs, &recorded, BTCB2_TIP).err(),
            Some(Error::NotBitcoinBlake2b(chain))
        );
    }
    let mut testnet = fixture.inputs();
    testnet.chain = ChainId::BitcoinBlake2bTestnet4;
    assert_eq!(
        create(&testnet).unwrap().chain(),
        ChainId::BitcoinBlake2bTestnet4
    );

    for shape in [Shape::Wpkh, Shape::WshSortedMulti] {
        let fixture = Fixture::new(shape);
        let sweep = fixture.sweep();
        let signers = fixture.signers();
        let secp = secp256k1::Secp256k1::new();
        assert!(matches!(
            signers[0]
                .session
                .sign_unified(&unified(sweep.psbt()), ChainId::Bitcoin, &secp),
            Err(ForeignUnifiedError::NotBitcoinBlake2b(ChainId::Bitcoin))
        ));

        let mut requested = sweep.psbt().clone();
        requested.inputs[0].sighash_type = Some(PsbtSighashType::from_u32(0x01));
        assert!(matches!(
            signers[0]
                .session
                .sign_unified(&unified(&requested), BTCB2, &secp),
            Err(ForeignUnifiedError::IncompatibleSighash {
                input: 0,
                actual: 0x01
            })
        ));

        let signed = sign_unified(sweep.psbt(), &signers);
        assert!(finalize(&sweep, &signed).is_ok());
        for (input, request) in [
            (0, Some(0x01)),
            (1, Some(0x01)),
            (0, Some(0x81)),
            (0, Some(0x02)),
            (0, Some(0x03)),
            (0, Some(0xa1)),
            (1, None),
        ] {
            let mut changed = signed.clone();
            changed.psbt_mut().inputs[input].sighash_type = request.map(PsbtSighashType::from_u32);
            assert_eq!(
                finalize(&sweep, &changed).err(),
                Some(UnsupportedSighash { input }),
                "{shape:?} input {input} request {request:?}"
            );
        }
    }
}

/// Finalization accepts only the exact construction plus unified records;
/// construction refuses a target that is not a Vault address type or is one
/// of the foreign wallet's own scripts, and out-of-bound fees and locktimes;
/// reconstruction rebuilds only the exact record; and the retained-witness
/// twin refuses a changed output or a witness from another construction.
#[test]
fn unified_sweep_refuses_changed_construction_target_and_source_script() {
    use UnifiedSweepFinalizeError::{ConstructionChanged, InvalidWitness};
    for shape in SHAPES {
        let fixture = Fixture::new(shape);
        let sweep = fixture.sweep();
        let signed = sign_unified(sweep.psbt(), &fixture.signers());
        assert!(finalize(&sweep, &signed).is_ok());

        let other_script = ScriptBuf::new_p2wsh(&ScriptBuf::new().wscript_hash());
        let changes: Vec<(&str, Change)> = vec![
            (
                "output value",
                Box::new(|p| p.unsigned_tx.output[0].value -= Amount::from_sat(1)),
            ),
            (
                "output script",
                Box::new({
                    let other = other_script.clone();
                    move |p| p.unsigned_tx.output[0].script_pubkey = other.clone()
                }),
            ),
            (
                "extra output",
                Box::new({
                    let other = other_script.clone();
                    move |p| {
                        p.unsigned_tx.output.push(TxOut {
                            value: Amount::from_sat(1_000),
                            script_pubkey: other.clone(),
                        });
                        p.outputs.push(Default::default());
                    }
                }),
            ),
            (
                "locktime",
                Box::new(|p| p.unsigned_tx.lock_time = LockTime::from_height(1).unwrap()),
            ),
            (
                "sequence",
                Box::new(|p| p.unsigned_tx.input[0].sequence = Sequence::MAX),
            ),
            (
                "dropped previous transaction",
                Box::new(|p| p.inputs[0].non_witness_utxo = None),
            ),
            (
                "dropped key origins",
                Box::new(|p| p.inputs[1].bip32_derivation.clear()),
            ),
            (
                "unknown field",
                Box::new(|p| {
                    p.inputs[0].unknown.insert(
                        bitcoin::psbt::raw::Key {
                            type_value: 0xf0,
                            key: vec![1],
                        },
                        vec![2],
                    );
                }),
            ),
        ];
        for (name, change) in &changes {
            let mut changed = signed.clone();
            change(changed.psbt_mut());
            assert_eq!(
                finalize(&sweep, &changed).err(),
                Some(ConstructionChanged),
                "{shape:?}: {name}"
            );
        }

        // Targets: only P2WSH and P2TR, and never the wallet's own script.
        let key = bitcoin::PublicKey::from_str(
            "02c6047f9441ed7d6d3045406e95c07cd85c778e4b8cef3ca7abac09b95c709ee5",
        )
        .unwrap();
        let hash = key.wpubkey_hash().unwrap();
        for bad in [
            ScriptBuf::new_p2wpkh(&hash),
            ScriptBuf::new_p2pkh(&key.pubkey_hash()),
            ScriptBuf::new_p2sh(&ScriptBuf::new_p2wpkh(&hash).script_hash()),
            ScriptBuf::new_op_return(bitcoin::script::PushBytesBuf::try_from(vec![1]).unwrap()),
        ] {
            let mut inputs = fixture.inputs();
            inputs.target = &bad;
            assert_eq!(
                create(&inputs).err(),
                Some(Error::InvalidTarget),
                "{shape:?}"
            );
        }
        if matches!(shape, Shape::WshSortedMulti | Shape::WshMulti) {
            for (branch, index) in [
                (SplitBranch::External, 0),
                (SplitBranch::External, 99),
                (SplitBranch::Internal, 3),
                (SplitBranch::Internal, 50),
            ] {
                let own = fixture
                    .wallet
                    .source
                    .derive(branch, index)
                    .unwrap()
                    .script_pubkey();
                let mut inputs = fixture.inputs();
                inputs.target = &own;
                assert_eq!(create(&inputs).err(), Some(Error::InvalidTarget));
            }
        }
        let taproot =
            ScriptBuf::new_p2tr_tweaked(bitcoin::key::TweakedPublicKey::dangerous_assume_tweaked(
                bitcoin::key::XOnlyPublicKey::from(key.inner),
            ));
        let mut inputs = fixture.inputs();
        inputs.target = &taproot;
        assert!(create(&inputs).is_ok());

        // Fee rate and locktime bounds.
        let inputs = fixture.inputs();
        for feerate in [0, spend::MAX_FEERATE + 1] {
            assert_eq!(
                create_unified_sweep(&inputs, feerate, LockTime::ZERO, BTCB2_TIP).err(),
                Some(Error::Economics)
            );
        }
        assert_eq!(
            create_unified_sweep(
                &inputs,
                FEERATE,
                LockTime::from_height(BTCB2_TIP + 1).unwrap(),
                BTCB2_TIP
            )
            .err(),
            Some(Error::Locktime)
        );

        // Reconstruction.
        let recorded = sweep.psbt().unsigned_tx.clone();
        assert_eq!(
            reconstruct_unified_sweep(&inputs, &recorded, BTCB2_TIP)
                .unwrap()
                .psbt(),
            sweep.psbt()
        );
        // Only the amount and locktime are read from the record, so another
        // amount within bounds rebuilds; one outside them is refused.
        let mut other_value = recorded.clone();
        other_value.output[0].value -= Amount::from_sat(1);
        assert_eq!(
            reconstruct_unified_sweep(&inputs, &other_value, BTCB2_TIP)
                .unwrap()
                .psbt()
                .unsigned_tx,
            other_value
        );
        let mut no_fee = recorded.clone();
        no_fee.output[0].value = Amount::from_sat(150_000);
        assert_eq!(
            reconstruct_unified_sweep(&inputs, &no_fee, BTCB2_TIP).err(),
            Some(Error::Economics)
        );
        let mut other_input = recorded.clone();
        other_input.input[0].previous_output.vout += 1;
        assert!(matches!(
            reconstruct_unified_sweep(&inputs, &other_input, BTCB2_TIP),
            Err(Error::Recorded(_))
        ));
        let mut other_target = recorded.clone();
        other_target.output[0].script_pubkey = other_script.clone();
        assert!(matches!(
            reconstruct_unified_sweep(&inputs, &other_target, BTCB2_TIP),
            Err(Error::Recorded(_))
        ));
        let mut two_outputs = recorded.clone();
        two_outputs.output.push(two_outputs.output[0].clone());
        assert!(matches!(
            reconstruct_unified_sweep(&inputs, &two_outputs, BTCB2_TIP),
            Err(Error::Recorded(_))
        ));
        let mut late = recorded.clone();
        late.lock_time = LockTime::from_height(BTCB2_TIP + 1).unwrap();
        assert_eq!(
            reconstruct_unified_sweep(&inputs, &late, BTCB2_TIP).err(),
            Some(Error::Locktime)
        );

        // The retained-witness twin.
        let tx = finalize(&sweep, &signed).unwrap().transaction().clone();
        let mut changed = tx.clone();
        changed.output[0].value -= Amount::from_sat(1);
        assert_eq!(verify(&sweep, &changed).err(), Some(ConstructionChanged));
        let mut stripped = tx.clone();
        stripped.input[0].script_sig = ScriptBuf::new();
        stripped.input[0].witness.clear();
        assert_eq!(verify(&sweep, &stripped).err(), Some(InvalidWitness));
        let elsewhere = vault_target(8..=10);
        let mut other_inputs = fixture.inputs();
        other_inputs.target = &elsewhere;
        let other_sweep = create(&other_inputs).unwrap();
        let other_tx = finalize(
            &other_sweep,
            &sign_unified(other_sweep.psbt(), &fixture.signers()),
        )
        .unwrap()
        .transaction()
        .clone();
        let mut spliced = tx.clone();
        spliced.input[0].script_sig = other_tx.input[0].script_sig.clone();
        spliced.input[0].witness = other_tx.input[0].witness.clone();
        assert_eq!(
            verify(&sweep, &spliced).err(),
            Some(InvalidWitness),
            "{shape:?}"
        );
    }
}

/// #647 O4 (Reviewer-647 probe D): a signed unified PSBT may add only the
/// reserved `coincube`/0 signature records. Any other proprietary record,
/// another namespace or another subtype of ours, is a changed construction,
/// as step 2 allows nothing but `partial_sigs`.
#[test]
fn unified_construction_refuses_foreign_proprietary_records() {
    use bitcoin::psbt::raw::ProprietaryKey;
    for shape in SHAPES {
        let fixture = Fixture::new(shape);
        let sweep = fixture.sweep();
        let signed = sign_unified(sweep.psbt(), &fixture.signers());
        assert!(finalize(&sweep, &signed).is_ok(), "{shape:?}");
        assert!(signed.psbt().inputs[0]
            .proprietary
            .keys()
            .all(|key| key.prefix == b"coincube" && key.subtype == 0));
        for (prefix, subtype) in [(&b"other"[..], 0u8), (&b"coincube"[..], 1u8)] {
            let mut changed = signed.clone();
            changed.psbt_mut().inputs[0].proprietary.insert(
                ProprietaryKey {
                    prefix: prefix.to_vec(),
                    subtype,
                    key: vec![1],
                },
                vec![2],
            );
            assert_eq!(
                finalize(&sweep, &changed).err(),
                Some(UnifiedSweepFinalizeError::ConstructionChanged),
                "{shape:?}: {prefix:?}/{subtype}"
            );
        }
    }
}
