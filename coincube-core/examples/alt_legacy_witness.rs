//! Synthetic 2-of-3 P2WSH helper for coincube#398 two-chain consensus probes.
//!
//! This is not the production Vault descriptor (that lives in the
//! `unified_finalize` unit tests). It only emits transaction hex for a Knots
//! pair: SHA with no activation override, BLAKE2b with
//! `-testactivationheight=blake2b@102`.
//!
//! Usage:
//!   cargo run -p coincube-core --example alt_legacy_witness -- template
//!   cargo run -p coincube-core --example alt_legacy_witness -- sign < funding.hex

use std::{
    io::{self, Read},
    str::FromStr,
};

use coincube_core::{
    bip39,
    miniscript::bitcoin::{
        absolute,
        bip32::DerivationPath,
        consensus::{deserialize, serialize},
        opcodes::all::{OP_CHECKMULTISIG, OP_PUSHNUM_2, OP_PUSHNUM_3},
        psbt::Psbt,
        script::Builder,
        secp256k1,
        sighash::{EcdsaSighashType, SighashCache},
        transaction, Address, Amount, Network, OutPoint, PublicKey, ScriptBuf, Sequence,
        Transaction, TxIn, TxOut, Witness,
    },
    psbt_unified::{unified_signatures, UnifiedPsbt},
    signer::MasterSigner,
    unified_signing::sign_p2wsh_all_unified,
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Vec<u8> {
    let s = s.trim();
    assert!(s.len().is_multiple_of(2), "odd hex length");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn witness_hex(tx: &Transaction, signatures: &[Vec<u8>], script: &ScriptBuf) -> String {
    let mut stack: Vec<Vec<u8>> = vec![vec![]];
    stack.extend(signatures.iter().cloned());
    stack.push(script.as_bytes().to_vec());
    let mut out = tx.clone();
    out.input[0].witness = Witness::from_slice(&stack);
    hex(&serialize(&out))
}

fn main() {
    let secp = secp256k1::Secp256k1::new();
    let path = DerivationPath::from_str("m/48'/0'/0/7").unwrap();
    let signers: Vec<_> = (1u8..=3)
        .map(|v| {
            MasterSigner::from_mnemonic(
                Network::Regtest,
                bip39::Mnemonic::from_entropy(&[v; 16]).unwrap(),
            )
            .unwrap()
        })
        .collect();
    let keys: Vec<PublicKey> = signers
        .iter()
        .map(|s| s.xpriv_at(&path, &secp).to_priv().public_key(&secp))
        .collect();
    let ws = Builder::new()
        .push_opcode(OP_PUSHNUM_2)
        .push_key(&keys[0])
        .push_key(&keys[1])
        .push_key(&keys[2])
        .push_opcode(OP_PUSHNUM_3)
        .push_opcode(OP_CHECKMULTISIG)
        .into_script();
    let spk = ws.to_p2wsh();
    let address = Address::from_script(&spk, Network::Regtest).unwrap();

    if std::env::args().nth(1).as_deref() == Some("template") {
        println!(
            "{}",
            serde_json::json!({
                "address": address.to_string(),
                "script": hex(ws.as_bytes()),
            })
        );
        return;
    }

    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap();
    let prev: Transaction = deserialize(&unhex(&input)).unwrap();
    let (vout, prevout) = prev
        .output
        .iter()
        .enumerate()
        .find(|(_, out)| out.script_pubkey == spk)
        .expect("funding output paying the 2-of-3");
    let tx = Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint {
                txid: prev.compute_txid(),
                vout: vout as u32,
            },
            script_sig: ScriptBuf::new(),
            sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(prevout.value.to_sat() - 1000),
            script_pubkey: spk,
        }],
    };
    let mut raw = Psbt::from_unsigned_tx(tx.clone()).unwrap();
    raw.inputs[0].witness_utxo = Some(prevout.clone());
    raw.inputs[0].non_witness_utxo = Some(prev.clone());
    raw.inputs[0].witness_script = Some(ws.clone());
    for (signer, key) in signers.iter().zip(&keys) {
        raw.inputs[0]
            .bip32_derivation
            .insert(key.inner, (signer.fingerprint(&secp), path.clone()));
    }

    let mut wrapped = UnifiedPsbt::from_psbt(raw).unwrap();
    wrapped = sign_p2wsh_all_unified(&signers[0], &wrapped, &secp).unwrap();
    wrapped = sign_p2wsh_all_unified(&signers[1], &wrapped, &secp).unwrap();
    let records = unified_signatures(&wrapped).unwrap();
    assert_eq!(records.len(), 2);
    let unified_sigs: Vec<Vec<u8>> = keys
        .iter()
        .take(2)
        .map(|key| {
            records
                .iter()
                .find(|r| r.public_key == *key)
                .unwrap()
                .signature
                .clone()
        })
        .collect();

    let digest = SighashCache::new(&tx)
        .p2wsh_signature_hash(0, &ws, prevout.value, EcdsaSighashType::All)
        .unwrap();
    let msg = secp256k1::Message::from_digest(*digest.as_ref());
    let legacy = |signer: &MasterSigner| {
        let sig = secp.sign_ecdsa_low_r(&msg, &signer.xpriv_at(&path, &secp).to_priv().inner);
        let mut bytes = sig.serialize_der().to_vec();
        bytes.push(1);
        bytes
    };
    let legacy_01 = vec![legacy(&signers[0]), legacy(&signers[1])];
    let legacy_12 = vec![legacy(&signers[1]), legacy(&signers[2])];
    let insufficient = [legacy(&signers[1])];

    let mut mixed = tx.clone();
    mixed.input[0].witness = Witness::from_slice(&[
        vec![],
        unified_sigs[0].clone(),
        legacy_01[1].clone(),
        ws.as_bytes().to_vec(),
    ]);
    let mut insufficient_tx = tx.clone();
    insufficient_tx.input[0].witness =
        Witness::from_slice(&[vec![], insufficient[0].clone(), ws.as_bytes().to_vec()]);
    let unified_hex = witness_hex(&tx, &unified_sigs, &ws);
    let alternate_hex = witness_hex(&tx, &legacy_12, &ws);
    let unified_tx: Transaction = deserialize(&unhex(&unified_hex)).unwrap();
    let alternate_tx: Transaction = deserialize(&unhex(&alternate_hex)).unwrap();

    println!(
        "{}",
        serde_json::json!({
            "txid": tx.compute_txid().to_string(),
            "unified_hex": unified_hex,
            "mixed_hex": hex(&serialize(&mixed)),
            "alternate_legacy_hex": alternate_hex,
            "same_keys_legacy_hex": witness_hex(&tx, &legacy_01, &ws),
            "insufficient_legacy_hex": hex(&serialize(&insufficient_tx)),
            "unified_wtxid": unified_tx.compute_wtxid().to_string(),
            "alternate_wtxid": alternate_tx.compute_wtxid().to_string(),
        })
    );
}
