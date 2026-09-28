//! Session-only PSBT preparation for the BTCB2 foreign-wallet Split tool.
//!
//! This module constructs and checks a signing handoff. It cannot finalize,
//! broadcast, persist secrets, or grant Claim/ancestry authority.

use std::{collections::BTreeSet, str::FromStr};

use coincube_core::{
    chain::ChainId,
    miniscript::{
        bitcoin::{
            absolute, psbt::Psbt, sighash::EcdsaSighashType, sighash::TapSighashType, transaction,
            Amount, OutPoint, Sequence, Transaction, TxIn, TxOut,
        },
        psbt::PsbtExt,
    },
    spend,
};
use sha2::{Digest, Sha256};

use super::foreign_scan::{Branch, ScanDescriptor, ScanReport};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForeignPsbtError {
    UnsupportedChain,
    StaleSession,
    SourceChanged,
    TargetChanged,
    Empty,
    DuplicateInput,
    Unconfirmed,
    Descriptor,
    Prevout,
    Economics,
    Psbt,
    Parse,
    ConstructionChanged,
    UnsupportedSighash,
    MissingSignature,
}

/// Explicit change selection. The amount is never inferred by the handoff.
pub struct ForeignChange {
    pub index: u32,
    pub amount: Amount,
}

/// Current UI/session identity supplied again at import time. Its descriptor
/// fingerprint is computed internally so callers cannot assert one by fiat.
pub struct ForeignSession<'a> {
    pub chain: ChainId,
    pub generation: u64,
    pub target_cube: &'a str,
    pub external: &'a ScanDescriptor,
    pub internal: Option<&'a ScanDescriptor>,
}

/// An imported partial-signature PSBT that remains evidence only. There is no
/// finalization or submission method on this type.
#[derive(Debug)]
pub struct VerifiedForeignPsbt(Psbt);
impl VerifiedForeignPsbt {
    pub fn psbt(&self) -> &Psbt {
        &self.0
    }
}

pub struct PreparedForeignSweep {
    original: Psbt,
    chain: ChainId,
    generation: u64,
    target_cube: String,
    source_fingerprint: [u8; 32],
    fee: Amount,
}

impl PreparedForeignSweep {
    /// Consume every authenticated coin in the report and construct one exact
    /// unsigned transaction. The caller supplies explicit destination, fee and
    /// optional change; their sum must equal the selected inputs exactly.
    pub fn new(
        report: &ScanReport,
        session: ForeignSession<'_>,
        destination: TxOut,
        fee: Amount,
        change: Option<ForeignChange>,
    ) -> Result<Self, ForeignPsbtError> {
        verify_session(report.chain(), report.generation(), &session)?;
        if report.coins().is_empty() {
            return Err(ForeignPsbtError::Empty);
        }
        if destination.value.to_sat() < spend::DUST_OUTPUT_SATS
            || destination.value > Amount::MAX_MONEY
            || fee == Amount::ZERO
            || fee > spend::MAX_FEE
        {
            return Err(ForeignPsbtError::Economics);
        }

        let mut coins: Vec<_> = report.coins().iter().collect();
        coins.sort_by_key(|coin| coin.outpoint);
        let mut seen = BTreeSet::<OutPoint>::new();
        let mut total = 0_u64;
        let mut inputs = Vec::with_capacity(coins.len());
        let mut derived = Vec::with_capacity(coins.len());
        for coin in coins {
            if !seen.insert(coin.outpoint) {
                return Err(ForeignPsbtError::DuplicateInput);
            }
            if !coin.confirmed {
                return Err(ForeignPsbtError::Unconfirmed);
            }
            let descriptor = descriptor_for(coin.branch, &session)?;
            let definite = descriptor
                .derive(coin.index)
                .map_err(|_| ForeignPsbtError::Descriptor)?;
            if definite.script_pubkey() != coin.output.script_pubkey {
                return Err(ForeignPsbtError::Descriptor);
            }
            let authenticated = spend::authenticate_previous_output(
                &coin.outpoint,
                Some(&coin.previous),
                Some(&coin.output),
            )
            .map_err(|_| ForeignPsbtError::Prevout)?;
            total = total
                .checked_add(authenticated.value.to_sat())
                .filter(|value| *value <= Amount::MAX_MONEY.to_sat())
                .ok_or(ForeignPsbtError::Economics)?;
            inputs.push(TxIn {
                previous_output: coin.outpoint,
                sequence: Sequence::ENABLE_RBF_NO_LOCKTIME,
                ..TxIn::default()
            });
            derived.push((definite, coin.previous.clone(), authenticated));
        }

        let mut outputs = vec![destination];
        let change_descriptor = match change {
            Some(change) => {
                let descriptor = session.internal.ok_or(ForeignPsbtError::Descriptor)?;
                let definite = descriptor
                    .derive(change.index)
                    .map_err(|_| ForeignPsbtError::Descriptor)?;
                if change.amount.to_sat() < spend::DUST_OUTPUT_SATS
                    || change.amount > Amount::MAX_MONEY
                {
                    return Err(ForeignPsbtError::Economics);
                }
                let script_pubkey = definite.script_pubkey();
                if script_pubkey == outputs[0].script_pubkey
                    || derived
                        .iter()
                        .any(|(_, _, previous)| previous.script_pubkey == script_pubkey)
                {
                    return Err(ForeignPsbtError::Economics);
                }
                outputs.push(TxOut {
                    value: change.amount,
                    script_pubkey,
                });
                Some(definite)
            }
            None => None,
        };
        let spent = outputs.iter().try_fold(fee.to_sat(), |sum, output| {
            sum.checked_add(output.value.to_sat())
        });
        if spent != Some(total) {
            return Err(ForeignPsbtError::Economics);
        }

        let tx = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: inputs,
            output: outputs,
        };
        let mut psbt = Psbt::from_unsigned_tx(tx).map_err(|_| ForeignPsbtError::Psbt)?;
        for (index, (descriptor, previous, output)) in derived.into_iter().enumerate() {
            psbt.inputs[index].non_witness_utxo = Some(previous);
            psbt.inputs[index].witness_utxo = Some(output);
            psbt.update_input_with_descriptor(index, &descriptor)
                .map_err(|_| ForeignPsbtError::Descriptor)?;
        }
        if let Some(descriptor) = change_descriptor {
            psbt.update_output_with_descriptor(1, &descriptor)
                .map_err(|_| ForeignPsbtError::Descriptor)?;
        }
        Ok(Self {
            original: psbt,
            chain: session.chain,
            generation: session.generation,
            target_cube: session.target_cube.to_owned(),
            source_fingerprint: source_fingerprint(&session)?,
            fee,
        })
    }

    pub fn export_text(&self) -> String {
        self.original.to_string()
    }

    pub fn fee(&self) -> Amount {
        self.fee
    }

    /// Parse a returned PSBT and allow only standard SIGHASH_ALL signatures to
    /// augment the exact prepared object. Signature validity and satisfaction
    /// are deliberately left to a later finalization boundary.
    pub fn import_text(
        &self,
        text: &str,
        current: ForeignSession<'_>,
    ) -> Result<VerifiedForeignPsbt, ForeignPsbtError> {
        if current.chain != ChainId::BitcoinBlake2b {
            return Err(ForeignPsbtError::UnsupportedChain);
        }
        if current.chain != self.chain || current.generation != self.generation {
            return Err(ForeignPsbtError::StaleSession);
        }
        if current.target_cube != self.target_cube {
            return Err(ForeignPsbtError::TargetChanged);
        }
        if source_fingerprint(&current)? != self.source_fingerprint {
            return Err(ForeignPsbtError::SourceChanged);
        }
        let signed = Psbt::from_str(text.trim()).map_err(|_| ForeignPsbtError::Parse)?;
        if signed.unsigned_tx != self.original.unsigned_tx
            || signed.inputs.len() != self.original.inputs.len()
            || signed.outputs.len() != self.original.outputs.len()
        {
            return Err(ForeignPsbtError::ConstructionChanged);
        }
        let mut normalized = signed.clone();
        let mut signatures = 0usize;
        for (index, input) in signed.inputs.iter().enumerate() {
            if input.partial_sigs.iter().any(|(key, signature)| {
                signature.sighash_type != EcdsaSighashType::All
                    || !input.bip32_derivation.contains_key(&key.inner)
            }) || input.tap_key_sig.is_some_and(|signature| {
                signature.sighash_type != TapSighashType::All || input.tap_internal_key.is_none()
            }) || input
                .sighash_type
                .is_some_and(|sighash| sighash.to_u32() != EcdsaSighashType::All as u32)
            {
                return Err(ForeignPsbtError::UnsupportedSighash);
            }
            signatures += input.partial_sigs.len() + usize::from(input.tap_key_sig.is_some());
            normalized.inputs[index].partial_sigs.clear();
            normalized.inputs[index].tap_key_sig = None;
            normalized.inputs[index].sighash_type = self.original.inputs[index].sighash_type;
        }
        if signatures == 0 {
            return Err(ForeignPsbtError::MissingSignature);
        }
        if normalized != self.original {
            return Err(ForeignPsbtError::ConstructionChanged);
        }
        Ok(VerifiedForeignPsbt(signed))
    }
}

fn verify_session(
    report_chain: ChainId,
    report_generation: u64,
    session: &ForeignSession<'_>,
) -> Result<(), ForeignPsbtError> {
    if report_chain != ChainId::BitcoinBlake2b || session.chain != ChainId::BitcoinBlake2b {
        return Err(ForeignPsbtError::UnsupportedChain);
    }
    if report_generation != session.generation {
        return Err(ForeignPsbtError::StaleSession);
    }
    if session.target_cube.is_empty()
        || session.external.branch() != Branch::External
        || session
            .internal
            .is_some_and(|descriptor| descriptor.branch() != Branch::Internal)
    {
        return Err(ForeignPsbtError::Descriptor);
    }
    Ok(())
}

fn descriptor_for<'a>(
    branch: Branch,
    session: &'a ForeignSession<'_>,
) -> Result<&'a ScanDescriptor, ForeignPsbtError> {
    match branch {
        Branch::External => Ok(session.external),
        Branch::Internal => session.internal.ok_or(ForeignPsbtError::Descriptor),
    }
}

fn source_fingerprint(session: &ForeignSession<'_>) -> Result<[u8; 32], ForeignPsbtError> {
    if session.external.branch() != Branch::External
        || session
            .internal
            .is_some_and(|descriptor| descriptor.branch() != Branch::Internal)
    {
        return Err(ForeignPsbtError::Descriptor);
    }
    let mut digest = Sha256::new();
    for (tag, descriptor) in [(0_u8, Some(session.external)), (1, session.internal)] {
        digest.update([tag]);
        match descriptor {
            Some(descriptor) => {
                let canonical = descriptor.canonical();
                digest.update((canonical.len() as u64).to_be_bytes());
                digest.update(canonical.as_bytes());
            }
            None => digest.update(0_u64.to_be_bytes()),
        }
    }
    Ok(digest.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{
        foreign_scan::{DiscoveredCoin, ScanReport},
        foreign_wallet_source::{SessionSeedSource, StandardSinglesig},
    };
    use coincube_core::miniscript::bitcoin::{self, hashes::Hash, secp256k1, BlockHash, PublicKey};
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };
    use zeroize::Zeroizing;

    const WORDS: &str =
        "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    fn fixture() -> (PreparedForeignSweep, SessionSeedSource, String) {
        let source = SessionSeedSource::new(
            Zeroizing::new(WORDS.to_owned()),
            Zeroizing::new("session passphrase".to_owned()),
        )
        .unwrap();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        let script = descriptors.external.script(7).unwrap();
        let previous = Transaction {
            version: transaction::Version::TWO,
            lock_time: absolute::LockTime::ZERO,
            input: vec![TxIn::default()],
            output: vec![TxOut {
                value: Amount::from_sat(100_000),
                script_pubkey: script,
            }],
        };
        let coin = DiscoveredCoin {
            branch: Branch::External,
            index: 7,
            outpoint: OutPoint::new(previous.compute_txid(), 0),
            output: previous.output[0].clone(),
            previous,
            confirmed: true,
        };
        let report = ScanReport::for_test(
            ChainId::BitcoinBlake2b,
            9,
            BlockHash::from_byte_array([2; 32]),
            vec![coin],
        );
        let destination = TxOut {
            value: Amount::from_sat(90_000),
            script_pubkey: bitcoin::ScriptBuf::new_p2wpkh(&bitcoin::WPubkeyHash::from_byte_array(
                [3; 20],
            )),
        };
        let prepared = PreparedForeignSweep::new(
            &report,
            ForeignSession {
                chain: ChainId::BitcoinBlake2b,
                generation: 9,
                target_cube: "vault-a",
                external: &descriptors.external,
                internal: Some(&descriptors.internal),
            },
            destination,
            Amount::from_sat(1_000),
            Some(ForeignChange {
                index: 4,
                amount: Amount::from_sat(9_000),
            }),
        )
        .unwrap();
        (prepared, source, "vault-a".to_owned())
    }

    fn signed_text(prepared: &PreparedForeignSweep) -> String {
        let mut psbt = Psbt::from_str(&prepared.export_text()).unwrap();
        let secret = secp256k1::SecretKey::from_slice(&[7; 32]).unwrap();
        let secp = secp256k1::Secp256k1::new();
        let signature = secp.sign_ecdsa(&secp256k1::Message::from_digest([8; 32]), &secret);
        let expected_key = *psbt.inputs[0].bip32_derivation.keys().next().unwrap();
        psbt.inputs[0].partial_sigs.insert(
            PublicKey::new(expected_key),
            bitcoin::ecdsa::Signature {
                signature,
                sighash_type: EcdsaSighashType::All,
            },
        );
        psbt.to_string()
    }

    #[test]
    fn signed_text_round_trips_and_preserves_bound_economics() {
        let (prepared, source, target) = fixture();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        let verified = prepared
            .import_text(
                &signed_text(&prepared),
                ForeignSession {
                    chain: ChainId::BitcoinBlake2b,
                    generation: 9,
                    target_cube: &target,
                    external: &descriptors.external,
                    internal: Some(&descriptors.internal),
                },
            )
            .unwrap();
        assert_eq!(prepared.fee(), Amount::from_sat(1_000));
        assert_eq!(
            verified.psbt().unsigned_tx.output[0].value,
            Amount::from_sat(90_000)
        );
        assert_eq!(
            verified.psbt().unsigned_tx.output[1].value,
            Amount::from_sat(9_000)
        );
    }

    #[test]
    fn transaction_and_metadata_tampering_refuse() {
        let (prepared, source, target) = fixture();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        let mut variants = Vec::new();
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.unsigned_tx.output[0].value = Amount::from_sat(1);
        variants.push(psbt);
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.unsigned_tx.output[1].script_pubkey = bitcoin::ScriptBuf::new();
        variants.push(psbt);
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.unsigned_tx.input[0].previous_output.vout = 1;
        variants.push(psbt);
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.inputs[0].witness_utxo.as_mut().unwrap().value = Amount::from_sat(1);
        variants.push(psbt);
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.inputs[0].non_witness_utxo = None;
        variants.push(psbt);
        let mut psbt = Psbt::from_str(&signed_text(&prepared)).unwrap();
        psbt.inputs[0].bip32_derivation.clear();
        variants.push(psbt);
        for variant in variants {
            assert!(prepared
                .import_text(
                    &variant.to_string(),
                    ForeignSession {
                        chain: ChainId::BitcoinBlake2b,
                        generation: 9,
                        target_cube: &target,
                        external: &descriptors.external,
                        internal: Some(&descriptors.internal)
                    }
                )
                .is_err());
        }
    }

    #[test]
    fn stale_target_chain_and_source_refuse() {
        let (prepared, source, target) = fixture();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        let text = signed_text(&prepared);
        assert_eq!(
            prepared
                .import_text(
                    &text,
                    ForeignSession {
                        chain: ChainId::BitcoinBlake2b,
                        generation: 10,
                        target_cube: &target,
                        external: &descriptors.external,
                        internal: Some(&descriptors.internal)
                    }
                )
                .unwrap_err(),
            ForeignPsbtError::StaleSession
        );
        assert_eq!(
            prepared
                .import_text(
                    &text,
                    ForeignSession {
                        chain: ChainId::BitcoinBlake2b,
                        generation: 9,
                        target_cube: "vault-b",
                        external: &descriptors.external,
                        internal: Some(&descriptors.internal)
                    }
                )
                .unwrap_err(),
            ForeignPsbtError::TargetChanged
        );
        let other = SessionSeedSource::new(
            Zeroizing::new(WORDS.to_owned()),
            Zeroizing::new("other".to_owned()),
        )
        .unwrap()
        .descriptors(StandardSinglesig::Bip84, 0)
        .unwrap();
        assert_eq!(
            prepared
                .import_text(
                    &text,
                    ForeignSession {
                        chain: ChainId::BitcoinBlake2b,
                        generation: 9,
                        target_cube: &target,
                        external: &other.external,
                        internal: Some(&other.internal)
                    }
                )
                .unwrap_err(),
            ForeignPsbtError::SourceChanged
        );
        assert_eq!(
            prepared
                .import_text(
                    &text,
                    ForeignSession {
                        chain: ChainId::Bitcoin,
                        generation: 9,
                        target_cube: &target,
                        external: &descriptors.external,
                        internal: Some(&descriptors.internal)
                    }
                )
                .unwrap_err(),
            ForeignPsbtError::UnsupportedChain
        );
    }

    #[test]
    fn seed_psbt_round_trip_never_writes_the_seed() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let datadir = std::env::temp_dir().join(format!("coincube-split-psbt-{unique}"));
        fs::create_dir(&datadir).unwrap();
        let (prepared, source, target) = fixture();
        let descriptors = source.descriptors(StandardSinglesig::Bip84, 0).unwrap();
        prepared
            .import_text(
                &signed_text(&prepared),
                ForeignSession {
                    chain: ChainId::BitcoinBlake2b,
                    generation: 9,
                    target_cube: &target,
                    external: &descriptors.external,
                    internal: Some(&descriptors.internal),
                },
            )
            .unwrap();
        drop(source);
        assert_eq!(fs::read_dir(&datadir).unwrap().count(), 0);
        fs::remove_dir(datadir).unwrap();
    }
}
