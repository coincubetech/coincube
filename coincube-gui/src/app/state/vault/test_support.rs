//! Shared fixtures for vault test modules.

use std::sync::Arc;

use coincube_core::miniscript::bitcoin::Psbt;
use tokio::sync::RwLock;

use crate::services::connect::client::auth::AccessTokenResponse;

/// An empty (no inputs/outputs) unsigned PSBT, suitable as a placeholder in tests.
pub fn empty_psbt() -> Psbt {
    use coincube_core::miniscript::bitcoin::{absolute, transaction, Transaction};

    Psbt::from_unsigned_tx(Transaction {
        version: transaction::Version::TWO,
        lock_time: absolute::LockTime::Blocks(absolute::Height::ZERO),
        input: vec![],
        output: vec![],
    })
    .expect("empty unsigned tx is psbt-compatible")
}

/// A shared, never-expiring set of Connect access tokens for tests.
pub fn tokens() -> Arc<RwLock<AccessTokenResponse>> {
    Arc::new(RwLock::new(AccessTokenResponse {
        access_token: "access".to_string(),
        refresh_token: "refresh".to_string(),
        expires_at: i64::MAX,
    }))
}

/// A Bitcoin Blake2b-shaped Vault fixture for the replay model: a 2-of-3
/// primary path over three deterministic hot signers plus a single-key
/// recovery leaf (`older(46)`, signer 2's second key), and an unsigned
/// one-input PSBT spending a coin of that Vault with an authenticated prevout
/// and its witness script. The same shape `coincubed`'s chain-keyed spend
/// tests use, so the desktop and the daemon are exercised on one fixture.
pub mod unified {
    use std::str::FromStr;

    use coincube_core::{
        descriptors::{CoincubeDescriptor, CoincubePolicy, PathInfo},
        miniscript::{
            bitcoin::{
                self as btc, absolute, bip32, bip32::DerivationPath, secp256k1, transaction,
                Amount, OutPoint, Psbt, ScriptBuf, Sequence, Transaction, TxIn, TxOut,
            },
            descriptor::{DerivPaths, DescriptorMultiXKey, DescriptorPublicKey, Wildcard},
        },
        psbt_unified::UnifiedPsbt,
        signer::MasterSigner,
        unified_signing::sign_p2wsh_all_unified,
    };

    pub fn signer(byte: u8) -> MasterSigner {
        let mnemonic = coincube_core::bip39::Mnemonic::from_entropy(&[byte; 16]).unwrap();
        MasterSigner::from_mnemonic(btc::Network::Bitcoin, mnemonic).unwrap()
    }

    fn descriptor_key(
        signer: &MasterSigner,
        branch: u32,
        secp: &secp256k1::Secp256k1<secp256k1::All>,
    ) -> DescriptorPublicKey {
        let origin = DerivationPath::from(vec![
            bip32::ChildNumber::from_hardened_idx(48).unwrap(),
            bip32::ChildNumber::from_hardened_idx(branch).unwrap(),
        ]);
        DescriptorPublicKey::MultiXPub(DescriptorMultiXKey {
            origin: Some((signer.fingerprint(secp), origin.clone())),
            xkey: signer.xpub_at(&origin, secp),
            derivation_paths: DerivPaths::new(vec![
                DerivationPath::from_str("m/0").unwrap(),
                DerivationPath::from_str("m/1").unwrap(),
            ])
            .unwrap(),
            wildcard: Wildcard::Unhardened,
        })
    }

    pub struct Fixture {
        pub descriptor: CoincubeDescriptor,
        pub signers: Vec<MasterSigner>,
        pub psbt: Psbt,
    }

    pub fn fixture() -> Fixture {
        let secp = secp256k1::Secp256k1::new();
        let signers = vec![signer(21), signer(22), signer(23)];
        let primary = PathInfo::Multi(
            2,
            vec![
                descriptor_key(&signers[0], 0, &secp),
                descriptor_key(&signers[1], 0, &secp),
                descriptor_key(&signers[2], 0, &secp),
            ],
        );
        let recovery = PathInfo::Single(descriptor_key(&signers[2], 1, &secp));
        let descriptor = CoincubeDescriptor::new(
            CoincubePolicy::new_legacy(primary, [(46, recovery)].iter().cloned().collect())
                .unwrap(),
        );
        let derived = descriptor.receive_descriptor().derive(3.into(), &secp);
        let output = TxOut {
            value: Amount::from_sat(50_000),
            script_pubkey: derived.script_pubkey(),
        };
        let previous_tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint::null(),
                script_sig: ScriptBuf::new(),
                sequence: Sequence(7),
                witness: btc::Witness::new(),
            }],
            output: vec![output.clone()],
        };
        let unsigned_tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn {
                previous_output: OutPoint {
                    txid: previous_tx.compute_txid(),
                    vout: 0,
                },
                script_sig: ScriptBuf::new(),
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                witness: btc::Witness::new(),
            }],
            output: vec![TxOut {
                value: Amount::from_sat(40_000),
                script_pubkey: ScriptBuf::new_p2wsh(&ScriptBuf::new().wscript_hash()),
            }],
        };
        let mut psbt = Psbt::from_unsigned_tx(unsigned_tx).unwrap();
        derived.update_psbt_in(&mut psbt.inputs[0]);
        psbt.inputs[0].non_witness_utxo = Some(previous_tx);
        psbt.inputs[0].witness_utxo = Some(output);
        Fixture {
            descriptor,
            signers,
            psbt,
        }
    }

    /// The same Vault as [`fixture`], spending **two** coins of it in one
    /// PSBT (indices 3 and 4 of the receive descriptor).
    ///
    /// The single-input fixture cannot express the per-input question the
    /// replay model answers: [`crate::app::state::vault::replay::ReplayStatus`]
    /// is `Replayable` when *any* input lacks a replay-capable signature, and a
    /// one-input PSBT makes "this input" and "the transaction" the same thing.
    pub fn two_input_fixture() -> Fixture {
        let secp = secp256k1::Secp256k1::new();
        let signers = vec![signer(21), signer(22), signer(23)];
        let primary = PathInfo::Multi(
            2,
            vec![
                descriptor_key(&signers[0], 0, &secp),
                descriptor_key(&signers[1], 0, &secp),
                descriptor_key(&signers[2], 0, &secp),
            ],
        );
        let recovery = PathInfo::Single(descriptor_key(&signers[2], 1, &secp));
        let descriptor = CoincubeDescriptor::new(
            CoincubePolicy::new_legacy(primary, [(46, recovery)].iter().cloned().collect())
                .unwrap(),
        );

        // Two coins of the same Vault at different derivation indices, each in
        // its own funding transaction, so the two inputs are independent.
        let mut previous_txs = Vec::new();
        let mut outputs = Vec::new();
        for (index, sats) in [(3u32, 50_000u64), (4, 60_000)] {
            let derived = descriptor.receive_descriptor().derive(index.into(), &secp);
            let output = TxOut {
                value: Amount::from_sat(sats),
                script_pubkey: derived.script_pubkey(),
            };
            previous_txs.push(Transaction {
                version: transaction::Version::TWO,
                lock_time: absolute::LockTime::ZERO,
                input: vec![TxIn {
                    previous_output: OutPoint::null(),
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence(7 + index),
                    witness: btc::Witness::new(),
                }],
                output: vec![output.clone()],
            });
            outputs.push((index, output));
        }

        let unsigned_tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: previous_txs
                .iter()
                .map(|tx| TxIn {
                    previous_output: OutPoint {
                        txid: tx.compute_txid(),
                        vout: 0,
                    },
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                    witness: btc::Witness::new(),
                })
                .collect(),
            output: vec![TxOut {
                value: Amount::from_sat(100_000),
                script_pubkey: ScriptBuf::new_p2wsh(&ScriptBuf::new().wscript_hash()),
            }],
        };
        let mut psbt = Psbt::from_unsigned_tx(unsigned_tx).unwrap();
        for (input_index, (deriv_index, output)) in outputs.into_iter().enumerate() {
            let derived = descriptor
                .receive_descriptor()
                .derive(deriv_index.into(), &secp);
            derived.update_psbt_in(&mut psbt.inputs[input_index]);
            psbt.inputs[input_index].non_witness_utxo = Some(previous_txs[input_index].clone());
            psbt.inputs[input_index].witness_utxo = Some(output);
        }
        Fixture {
            descriptor,
            signers,
            psbt,
        }
    }

    /// `psbt` with `signer`'s unified signatures added (proprietary records).
    pub fn unified(psbt: &Psbt, signer: &MasterSigner) -> Psbt {
        let secp = secp256k1::Secp256k1::new();
        let u = UnifiedPsbt::from_psbt(psbt.clone()).unwrap();
        sign_p2wsh_all_unified(signer, &u, &secp)
            .unwrap()
            .psbt()
            .clone()
    }

    /// `psbt` with `signer`'s legacy `SIGHASH_ALL` signatures added
    /// (`partial_sigs`, the ordinary Bitcoin path).
    pub fn legacy(psbt: &Psbt, signer: &MasterSigner) -> Psbt {
        let secp = secp256k1::Secp256k1::new();
        signer.sign_psbt(psbt.clone(), &secp).unwrap()
    }
}
