//! Upstream coverage, stated plainly: Knots' `unified_sighash.json` vectors
//! (checked in `unified_sighash.rs`) use synthetic scriptCodes and outputs, so
//! they pin the digest but not *which* scriptCode and script type each foreign
//! input kind uses. For that there is no static upstream vector. The tests
//! here are self-consistency plus an independent cross-check: the scriptCode
//! and script type are rebuilt by hand from the final transaction bytes per
//! `doc/unified-sighash.md` at `v29.4.1.knots20260508`, hashed with the
//! vector-backed [`unified_sighash`], and every witness is replayed through
//! rust-miniscript's interpreter with that digest. Acceptance by a 29.4.1
//! regtest node (and rejection by a pre-fork one) is still required before
//! any of this is called verified against upstream.

use std::str::FromStr;

use miniscript::{
    bitcoin::{
        absolute,
        bip32::{DerivationPath, Fingerprint},
        psbt::Psbt,
        script::Instruction,
        sighash::{EcdsaSighashType, SighashCache},
        transaction, Amount, Network, OutPoint, Script, Sequence, Transaction, TxIn,
    },
    interpreter::{Interpreter, KeySigPair},
    psbt::PsbtExt,
    DefiniteDescriptorKey, Descriptor, DescriptorPublicKey,
};

use super::*;
use crate::{
    psbt_unified::{export_standard, import_standard},
    unified_sighash::unified_sighash,
};

fn session(byte: u8) -> SessionSigner {
    let mnemonic = bip39::Mnemonic::from_entropy(&[byte; 16]).unwrap();
    SessionSigner::from_mnemonic(Network::Bitcoin, mnemonic, "").unwrap()
}

fn key(signer: &SessionSigner, purpose: u32) -> String {
    let secp = secp256k1::Secp256k1::new();
    let origin = DerivationPath::from_str(&format!("m/{purpose}'/0'/0'")).unwrap();
    let fingerprint: Fingerprint = signer.fingerprint(&secp);
    let xpub = signer.xpub_at(&origin, &secp);
    format!("[{fingerprint}/{purpose}'/0'/0']{xpub}/0/*")
}

fn descriptor(template: &str) -> Descriptor<DefiniteDescriptorKey> {
    Descriptor::<DescriptorPublicKey>::from_str(template)
        .unwrap()
        .at_derivation_index(3)
        .unwrap()
}

/// One input per descriptor, each funded by its own previous transaction.
fn psbt_for(descriptors: &[Descriptor<DefiniteDescriptorKey>]) -> UnifiedPsbt {
    let mut previous = Vec::new();
    for (index, descriptor) in descriptors.iter().enumerate() {
        previous.push(Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::new(),
                sequence: Sequence(index as u32 + 1),
                witness: Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(50_000 + index as u64),
                script_pubkey: descriptor.script_pubkey(),
            }],
        });
    }
    let unsigned = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: previous
            .iter()
            .map(|tx| TxIn {
                previous_output: OutPoint::new(tx.compute_txid(), 0),
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: Witness::new(),
            })
            .collect(),
        output: vec![TxOut {
            value: Amount::from_sat(40_000),
            script_pubkey: ScriptBuf::new_p2wsh(&ScriptBuf::new().wscript_hash()),
        }],
    };
    let mut psbt = Psbt::from_unsigned_tx(unsigned).unwrap();
    for (index, (tx, descriptor)) in previous.into_iter().zip(descriptors).enumerate() {
        psbt.inputs[index].witness_utxo = Some(tx.output[0].clone());
        psbt.inputs[index].non_witness_utxo = Some(tx);
        psbt.update_input_with_descriptor(index, descriptor)
            .unwrap();
    }
    UnifiedPsbt::from_psbt(psbt).unwrap()
}

fn spent_outputs(psbt: &UnifiedPsbt) -> Vec<TxOut> {
    psbt.psbt()
        .inputs
        .iter()
        .map(|input| input.witness_utxo.clone().unwrap())
        .collect()
}

fn blake2b_sign(signer: &SessionSigner, psbt: &UnifiedPsbt) -> UnifiedPsbt {
    let secp = secp256k1::Secp256k1::new();
    signer
        .sign_unified(psbt, ChainId::BitcoinBlake2b, &secp)
        .unwrap()
}

/// scriptCode and script type rebuilt from the final bytes alone, per the
/// Knots doc: P2PKH signs its scriptPubKey as type 0; P2WPKH and P2SH-P2WPKH
/// sign `76 a9 14 <hash> 88 ac` as type 1; P2WSH signs its witnessScript as
/// type 1. Deliberately not shared with the module under test.
fn independent_script_code(spk: &Script, script_sig: &Script, witness: &Witness) -> (u8, Vec<u8>) {
    let p2pkh = |hash: &[u8]| [&[0x76, 0xa9, 0x14][..], hash, &[0x88, 0xac]].concat();
    let bytes = spk.as_bytes();
    match bytes {
        [0x76, 0xa9, 0x14, .., 0x88, 0xac] if bytes.len() == 25 => (0, bytes.to_vec()),
        [0x00, 0x14, hash @ ..] if hash.len() == 20 => (1, p2pkh(hash)),
        [0x00, 0x20, ..] if bytes.len() == 34 => (1, witness.last().unwrap().to_vec()),
        [0xa9, 0x14, .., 0x87] if bytes.len() == 23 => {
            let Some(Ok(Instruction::PushBytes(redeem))) = script_sig.instructions().next() else {
                panic!("P2SH scriptSig must push the redeem script");
            };
            assert_eq!(redeem.as_bytes()[..2], [0x00, 0x14]);
            (1, p2pkh(&redeem.as_bytes()[2..]))
        }
        _ => panic!("unexpected scriptPubKey {}", spk),
    }
}

/// `DER || 0x21` elements swapped for `DER || 0x01`: rust-bitcoin cannot parse
/// the unified byte, and the interpreter only needs a parseable signature to
/// hand to the custom verifier, which checks the *unified* digest.
fn placeholder(element: &[u8]) -> Vec<u8> {
    let mut element = element.to_vec();
    if element.len() > 8 && element[0] == 0x30 && element.last() == Some(&0x21) {
        *element.last_mut().unwrap() = 0x01;
    }
    element
}

/// Replay every input of `spend` through the miniscript interpreter and verify
/// each signature it meets against the independently computed unified digest.
/// Returns, per input, the number of signatures verified.
fn independent_check(spend: &FinalizedSpend, spent: &[TxOut]) -> Vec<usize> {
    let secp = secp256k1::Secp256k1::verification_only();
    let tx = &spend.transaction;
    let mut counts = Vec::new();
    for (index, txin) in tx.input.iter().enumerate() {
        let spk = &spent[index].script_pubkey;
        let (script_type, code) = independent_script_code(spk, &txin.script_sig, &txin.witness);
        let digest = unified_sighash(
            tx,
            index,
            0x21,
            script_type,
            spent,
            &ScriptBuf::from_bytes(code),
        )
        .unwrap();
        let message = secp256k1::Message::from_digest(digest);

        for element in txin
            .witness
            .iter()
            .chain(
                txin.script_sig
                    .instructions()
                    .filter_map(|i| match i.unwrap() {
                        Instruction::PushBytes(p) => Some(p.as_bytes()),
                        _ => None,
                    }),
            )
        {
            if element.len() > 8 && element[0] == 0x30 {
                assert_eq!(element.last(), Some(&0x21), "every signature is unified");
            }
        }
        let script_sig = Builder::new();
        let script_sig = txin
            .script_sig
            .instructions()
            .fold(script_sig, |b, i| match i.unwrap() {
                Instruction::PushBytes(p) => b.push_slice(push_bytes(placeholder(p.as_bytes()))),
                Instruction::Op(op) => b.push_opcode(op),
            })
            .into_script();
        let witness =
            Witness::from_slice(&txin.witness.iter().map(placeholder).collect::<Vec<_>>());
        let interpreter =
            Interpreter::from_txdata(spk, &script_sig, &witness, txin.sequence, tx.lock_time)
                .unwrap();
        let mut verified = 0;
        for step in interpreter.iter_custom(Box::new(|pair: &KeySigPair| match pair {
            KeySigPair::Ecdsa(pk, sig) => {
                let ok = secp
                    .verify_ecdsa(&message, &sig.signature, &pk.inner)
                    .is_ok();
                verified += usize::from(ok);
                ok
            }
            KeySigPair::Schnorr(..) => false,
        })) {
            step.unwrap_or_else(|e| panic!("input {}: interpreter rejected witness: {}", index, e));
        }
        counts.push(verified);
    }
    counts
}

fn round_trip(psbt: &UnifiedPsbt, signers: &[&SessionSigner]) -> (UnifiedPsbt, FinalizedSpend) {
    let secp = secp256k1::Secp256k1::new();
    let mut signed = psbt.clone();
    for signer in signers {
        signed = blake2b_sign(signer, &signed);
    }
    let spend = finalize_foreign_unified(&signed, &secp).unwrap();
    assert!(spend.inputs.iter().all(|r| r.replay_protected()));
    let counts: Vec<_> = spend.inputs.iter().map(|r| r.unified_used).collect();
    assert_eq!(independent_check(&spend, &spent_outputs(psbt)), counts);
    (signed, spend)
}

#[test]
fn single_sig_types_sign_verify_and_finalize() {
    let a = session(1);
    let psbt = psbt_for(&[
        descriptor(&format!("wpkh({})", key(&a, 84))),
        descriptor(&format!("sh(wpkh({}))", key(&a, 49))),
        descriptor(&format!("pkh({})", key(&a, 44))),
    ]);
    let (signed, spend) = round_trip(&psbt, &[&a]);
    let secp = secp256k1::Secp256k1::verification_only();
    assert_eq!(verify_foreign_unified(&signed, &secp), Ok(3));
    for input in &signed.psbt().inputs {
        assert!(
            input.partial_sigs.is_empty(),
            "unified path never writes legacy"
        );
        assert_eq!(input.sighash_type.map(|s| s.to_u32()), Some(0x21));
    }
    let tx = &spend.transaction;
    assert!(tx.input[0].script_sig.is_empty() && tx.input[0].witness.len() == 2);
    assert_eq!(tx.input[1].script_sig.len(), 23); // push of the 22-byte program
    assert!(tx.input[2].witness.is_empty() && !tx.input[2].script_sig.is_empty());

    // The P2WPKH signature is not a Bitcoin (BIP143) signature.
    let record = &unified_signatures(&signed).unwrap()[0];
    let spent = spent_outputs(&psbt);
    let legacy = SighashCache::new(tx)
        .p2wpkh_signature_hash(
            0,
            &spent[0].script_pubkey,
            spent[0].value,
            EcdsaSighashType::All,
        )
        .unwrap();
    let der = secp256k1::ecdsa::Signature::from_der(record.signature.split_last().unwrap().1);
    assert!(secp
        .verify_ecdsa(
            &secp256k1::Message::from_digest(legacy.to_byte_array()),
            &der.unwrap(),
            &record.public_key.inner
        )
        .is_err());
}

#[test]
fn wsh_multi_and_sortedmulti_need_the_threshold() {
    let (a, b, c) = (session(1), session(2), session(3));
    for template in ["multi", "sortedmulti"] {
        let psbt = psbt_for(&[descriptor(&format!(
            "wsh({template}(2,{},{},{}))",
            key(&a, 48),
            key(&b, 48),
            key(&c, 48)
        ))]);
        let secp = secp256k1::Secp256k1::new();
        let one = blake2b_sign(&c, &psbt);
        assert_eq!(
            finalize_foreign_unified(&one, &secp),
            Err(ForeignUnifiedError::Unsatisfiable {
                input: 0,
                have: 1,
                need: 2
            })
        );
        let (_, spend) = round_trip(&psbt, &[&c, &a, &b]);
        assert_eq!(spend.inputs[0].unified_used, 2);
        assert_eq!(spend.transaction.input[0].witness.len(), 4); // dummy, 2 sigs, script
    }
}

#[test]
fn every_non_blake2b_chain_is_refused() {
    let a = session(1);
    let psbt = psbt_for(&[descriptor(&format!("wpkh({})", key(&a, 84)))]);
    let secp = secp256k1::Secp256k1::new();
    for chain in ChainId::ALL {
        let result = a.sign_unified(&psbt, chain, &secp);
        if chain.is_blake2b() {
            assert_eq!(verify_foreign_unified(&result.unwrap(), &secp), Ok(1));
        } else {
            assert_eq!(result, Err(ForeignUnifiedError::NotBitcoinBlake2b(chain)));
        }
    }
}

/// I5 regression guard, pinned from this commit (not an upstream answer): the
/// Bitcoin hot signer still emits the same `SIGHASH_ALL` bytes for a P2WSH
/// Vault input, with nothing in the unified namespace.
#[test]
fn bitcoin_signing_bytes_are_unchanged() {
    let secp = secp256k1::Secp256k1::new();
    let fixture = crate::unified_signing::tests::fixture(1);
    let signed = fixture.signers[0]
        .sign_psbt(fixture.psbt.psbt().clone(), &secp)
        .unwrap();
    let input = &signed.inputs[0];
    assert!(input.proprietary.is_empty() && input.sighash_type.is_none());
    let sigs: Vec<String> = input
        .partial_sigs
        .values()
        .map(|sig| sig.to_vec().iter().map(|b| format!("{b:02x}")).collect())
        .collect();
    assert_eq!(sigs, vec![PINNED_BITCOIN_SIGNATURE.to_string()]);
}

const PINNED_BITCOIN_SIGNATURE: &str = "304402202b12250c23858c7e9aef56f4a3ec7f56e76c4cfea94f995f8e2cbb8992bfc39502205e75e2091401a6ca85cd2d0d1e1d33ad56e53841ef9c47f8b59ee636467b14eb01";

#[test]
fn taproot_is_scan_only() {
    let a = session(1);
    let psbt = psbt_for(&[descriptor(&format!("tr({})", key(&a, 86)))]);
    let secp = secp256k1::Secp256k1::new();
    assert_eq!(
        a.sign_unified(&psbt, ChainId::BitcoinBlake2b, &secp),
        Err(ForeignUnifiedError::TaprootScanOnly { input: 0 })
    );
    assert_eq!(
        finalize_foreign_unified(&psbt, &secp),
        Err(ForeignUnifiedError::TaprootScanOnly { input: 0 })
    );
}

#[test]
fn unidentified_inputs_are_refused() {
    let (a, b) = (session(1), session(2));
    let secp = secp256k1::Secp256k1::new();
    let refused = |template: String| {
        let psbt = psbt_for(&[descriptor(&template)]);
        a.sign_unified(&psbt, ChainId::BitcoinBlake2b, &secp)
            .unwrap_err()
    };
    // A Vault-shaped (non-`multi`) P2WSH script belongs to `unified_signing`.
    assert!(matches!(
        refused(format!(
            "wsh(and_v(v:pk({}),pk({})))",
            key(&a, 48),
            key(&b, 48)
        )),
        ForeignUnifiedError::UnsupportedScript { input: 0, .. }
    ));
    assert!(matches!(
        refused(format!("sh(wsh(multi(1,{},{})))", key(&a, 48), key(&b, 48))),
        ForeignUnifiedError::UnsupportedScript { input: 0, .. }
    ));

    // A bare OP_TRUE output.
    let mut psbt = psbt_for(&[descriptor(&format!("wpkh({})", key(&a, 84)))]);
    let bare = ScriptBuf::from_bytes(vec![0x51]);
    let raw = psbt.psbt_mut();
    let prev = raw.inputs[0].non_witness_utxo.as_mut().unwrap();
    prev.output[0].script_pubkey = bare.clone();
    raw.unsigned_tx.input[0].previous_output.txid = prev.compute_txid();
    raw.inputs[0].witness_utxo.as_mut().unwrap().script_pubkey = bare;
    assert!(matches!(
        verify_foreign_unified(&psbt, &secp),
        Err(ForeignUnifiedError::UnsupportedScript { input: 0, .. })
    ));

    // No previous transaction: prevouts cannot be authenticated.
    let mut psbt = psbt_for(&[descriptor(&format!("wpkh({})", key(&a, 84)))]);
    psbt.psbt_mut().inputs[0].non_witness_utxo = None;
    assert!(matches!(
        verify_foreign_unified(&psbt, &secp),
        Err(ForeignUnifiedError::InputAuthentication { input: 0, .. })
    ));
}

#[test]
fn anyonecanpay_legacy_and_foreign_requests_are_refused() {
    let a = session(1);
    let secp = secp256k1::Secp256k1::new();
    let psbt = psbt_for(&[descriptor(&format!("wpkh({})", key(&a, 84)))]);

    for request in [0x81u32, 0xa1, 0x83] {
        let mut raw = psbt.psbt().clone();
        raw.inputs[0].sighash_type = Some(PsbtSighashType::from_u32(request));
        assert!(matches!(
            UnifiedPsbt::from_psbt(raw),
            Err(UnifiedPsbtError::UnsupportedSighashRequest { input: 0, .. })
        ));
    }
    // A legacy SIGHASH_ALL request is not silently upgraded.
    let mut legacy_request = psbt.clone();
    legacy_request.psbt_mut().inputs[0].sighash_type = Some(PsbtSighashType::from_u32(0x01));
    assert_eq!(
        a.sign_unified(&legacy_request, ChainId::BitcoinBlake2b, &secp),
        Err(ForeignUnifiedError::IncompatibleSighash {
            input: 0,
            actual: 0x01
        })
    );
    // A stored unified record carrying ANYONECANPAY is refused.
    let mut signed = blake2b_sign(&a, &psbt);
    for value in signed.psbt_mut().inputs[0].proprietary.values_mut() {
        *value.last_mut().unwrap() = 0xa1;
    }
    assert!(matches!(
        verify_foreign_unified(&signed, &secp),
        Err(ForeignUnifiedError::Adapter(
            UnifiedPsbtError::UnsupportedUnifiedSighash { input: 0, .. }
        ))
    ));
    // Any legacy partial signature keeps the input off this path.
    let mut with_legacy = psbt.clone();
    let public_key = PublicKey::new(
        *psbt.psbt().inputs[0]
            .bip32_derivation
            .keys()
            .next()
            .unwrap(),
    );
    let secret = secp256k1::SecretKey::from_slice(&[7u8; 32]).unwrap();
    let sig = miniscript::bitcoin::ecdsa::Signature::sighash_all(
        secp.sign_ecdsa(&secp256k1::Message::from_digest([9u8; 32]), &secret),
    );
    with_legacy.psbt_mut().inputs[0]
        .partial_sigs
        .insert(public_key, sig);
    assert_eq!(
        verify_foreign_unified(&with_legacy, &secp),
        Err(ForeignUnifiedError::LegacySignature { input: 0 })
    );
}

#[test]
fn wrong_seed_and_tampered_derivation_are_refused() {
    let (a, b) = (session(1), session(2));
    let secp = secp256k1::Secp256k1::new();
    let psbt = psbt_for(&[descriptor(&format!("wpkh({})", key(&a, 84)))]);
    assert_eq!(
        b.sign_unified(&psbt, ChainId::BitcoinBlake2b, &secp),
        Err(ForeignUnifiedError::NothingToSign)
    );
    let mut tampered = psbt.clone();
    for (_, path) in tampered.psbt_mut().inputs[0].bip32_derivation.values_mut() {
        *path = DerivationPath::from_str("m/84'/0'/0'/0/4").unwrap();
    }
    assert!(matches!(
        a.sign_unified(&tampered, ChainId::BitcoinBlake2b, &secp),
        Err(ForeignUnifiedError::DerivedPublicKeyMismatch { input: 0, .. })
    ));
    // The digest commits to the whole transaction: any change after signing
    // invalidates the stored record.
    let signed = blake2b_sign(&a, &psbt);
    let mut forged = signed.clone();
    forged.psbt_mut().unsigned_tx.lock_time = absolute::LockTime::from_consensus(1);
    assert!(matches!(
        verify_foreign_unified(&forged, &secp),
        Err(ForeignUnifiedError::InvalidUnifiedSignature { input: 0, .. })
    ));
}

#[test]
fn standard_export_preserves_unified_signature_bytes() {
    let a = session(1);
    let psbt = psbt_for(&[
        descriptor(&format!("wpkh({})", key(&a, 84))),
        descriptor(&format!("pkh({})", key(&a, 44))),
    ]);
    let signed = blake2b_sign(&a, &psbt);
    let exported = export_standard(&signed).unwrap();
    let imported = import_standard(&exported).unwrap();
    assert_eq!(imported, signed);
    let spend = finalize_foreign_unified(&imported, &secp256k1::Secp256k1::new()).unwrap();
    for record in unified_signatures(&signed).unwrap() {
        let txin = &spend.transaction.input[record.input_index];
        let placed = txin.witness.iter().any(|e| e == record.signature.as_slice())
            || txin.script_sig.instructions().any(|i| {
                matches!(i, Ok(Instruction::PushBytes(p)) if p.as_bytes() == record.signature.as_slice())
            });
        assert!(placed, "witness carries the stored DER || 0x21 bytes");
    }
}
