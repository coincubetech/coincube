//! Finalize an owned Bitcoin poison construction, not a Claim/broadcast permission.
//! Only standard SIGHASH_ALL partial signatures may augment the construction.
//! Existing Miniscript finalization and interpreter machinery verifies the actual
//! retained witness; no custom script interpreter or caller-supplied final tx.

use crate::{
    chain::ChainId, claim_spend::PoisonSelfTransfer, descriptors::CoincubeDescriptor, spend,
};
use miniscript::{
    bitcoin::{
        self,
        hashes::Hash,
        psbt::Psbt,
        secp256k1,
        sighash::{EcdsaSighashType, Prevouts, SighashCache},
        Amount, Transaction, Txid,
    },
    interpreter::{Interpreter, KeySigPair, SatisfiedConstraint},
    psbt::PsbtExt,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizeError {
    ConstructionChanged,
    UnsupportedSighash,
    InputAuthentication,
    InvalidSignature { input: usize },
    Economics,
    Unsatisfied,
    InvalidWitness,
}
impl std::fmt::Display for FinalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Poison transfer finalization refused: {:?}", self)
    }
}
impl std::error::Error for FinalizeError {}

/// Cryptographic evidence only. No deserialization/public-field bypass. Neither
/// signatures nor poison construction prove current RDTS activity, Bitcoin
/// inclusion, chain exclusivity, mempool acceptance, maturity or reorg safety.
#[derive(Debug)]
pub struct VerifiedPoisonTransfer {
    transaction: Transaction,
    chain: ChainId,
    descriptor: CoincubeDescriptor,
    construction_txid: Txid,
    fee: Amount,
    signatures_per_input: Vec<usize>,
}
impl VerifiedPoisonTransfer {
    pub fn transaction(&self) -> &Transaction {
        &self.transaction
    }
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    pub fn descriptor(&self) -> &CoincubeDescriptor {
        &self.descriptor
    }
    pub fn construction_txid(&self) -> Txid {
        self.construction_txid
    }
    pub fn fee(&self) -> Amount {
        self.fee
    }
    pub fn vsize(&self) -> usize {
        self.transaction.vsize()
    }
    pub fn signatures_per_input(&self) -> &[usize] {
        &self.signatures_per_input
    }
}

/// Accept only an unfinalized partial-signature PSBT from the exact opaque
/// construction. All metadata (including full prevouts, key origins, witness
/// scripts, outputs, proprietary/unknown maps) must remain identical; only
/// partial_sigs and an absent/ALL sighash request may differ. Signers that strip
/// construction data must merge their signatures back into that exact PSBT.
/// Prefinalized PSBTs and unified proprietary records intentionally refuse.
pub fn finalize_poison_transfer<C: secp256k1::Verification>(
    construction: &PoisonSelfTransfer,
    signed: &Psbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedPoisonTransfer, FinalizeError> {
    let original = construction.psbt();
    if signed.unsigned_tx != original.unsigned_tx
        || signed.inputs.len() != original.inputs.len()
        || signed.outputs.len() != original.outputs.len()
    {
        return Err(FinalizeError::ConstructionChanged);
    }
    let mut normalized = signed.clone();
    for (index, input) in signed.inputs.iter().enumerate() {
        if input
            .sighash_type
            .is_some_and(|s| s.ecdsa_hash_ty() != Ok(EcdsaSighashType::All))
            || input
                .partial_sigs
                .values()
                .any(|s| s.sighash_type != EcdsaSighashType::All)
        {
            return Err(FinalizeError::UnsupportedSighash);
        }
        normalized.inputs[index].partial_sigs.clear();
        normalized.inputs[index].sighash_type = original.inputs[index].sighash_type;
    }
    if normalized != *original {
        return Err(FinalizeError::ConstructionChanged);
    }
    spend::reverify_spend_before_broadcast(construction.descriptor(), signed)
        .map_err(|_| FinalizeError::Economics)?;
    let mut prevouts = Vec::with_capacity(signed.inputs.len());
    let mut cache = SighashCache::new(&signed.unsigned_tx);
    for (index, input) in signed.inputs.iter().enumerate() {
        let output = spend::authenticate_previous_output(
            &signed.unsigned_tx.input[index].previous_output,
            input.non_witness_utxo.as_ref(),
            input.witness_utxo.as_ref(),
        )
        .map_err(|_| FinalizeError::InputAuthentication)?;
        let script = input
            .witness_script
            .as_ref()
            .ok_or(FinalizeError::InputAuthentication)?;
        if !output.script_pubkey.is_p2wsh() || script.to_p2wsh() != output.script_pubkey {
            return Err(FinalizeError::InputAuthentication);
        }
        let hash = cache
            .p2wsh_signature_hash(index, script, output.value, EcdsaSighashType::All)
            .map_err(|_| FinalizeError::InvalidSignature { input: index })?;
        let message = secp256k1::Message::from_digest(hash.to_byte_array());
        // Check all supplied signatures, including any surplus not selected for
        // the final witness. Do not hide an invalid record behind a valid quorum.
        for (key, signature) in &input.partial_sigs {
            if !input.bip32_derivation.contains_key(&key.inner)
                || !key.compressed
                || secp
                    .verify_ecdsa(&message, &signature.signature, &key.inner)
                    .is_err()
            {
                return Err(FinalizeError::InvalidSignature { input: index });
            }
        }
        prevouts.push(output);
    }
    let fee = signed.fee().map_err(|_| FinalizeError::Economics)?;
    let mut finalized = signed.clone();
    finalized
        .finalize_mut(secp)
        .map_err(|_| FinalizeError::Unsatisfied)?;
    // extract() also runs the library interpreter; no unchecked extraction.
    let transaction = finalized
        .extract(secp)
        .map_err(|_| FinalizeError::InvalidWitness)?;
    let signatures_per_input = verify_retained_witness(&transaction, original, &prevouts, secp)?;
    Ok(VerifiedPoisonTransfer {
        construction_txid: original.unsigned_tx.compute_txid(),
        transaction,
        chain: construction.chain(),
        descriptor: construction.descriptor().clone(),
        fee,
        signatures_per_input,
    })
}

fn verify_retained_witness<C: secp256k1::Verification>(
    transaction: &Transaction,
    original: &Psbt,
    prevouts: &[bitcoin::TxOut],
    secp: &secp256k1::Secp256k1<C>,
) -> Result<Vec<usize>, FinalizeError> {
    let mut unsigned = transaction.clone();
    for input in &mut unsigned.input {
        if !input.script_sig.is_empty() {
            return Err(FinalizeError::InvalidWitness);
        }
        input.witness.clear();
    }
    if unsigned != original.unsigned_tx {
        return Err(FinalizeError::ConstructionChanged);
    }
    let mut counts = Vec::with_capacity(transaction.input.len());
    for (index, input) in transaction.input.iter().enumerate() {
        let interpreter = Interpreter::from_txdata(
            &prevouts[index].script_pubkey,
            &input.script_sig,
            &input.witness,
            input.sequence,
            transaction.lock_time,
        )
        .map_err(|_| FinalizeError::InvalidWitness)?;
        let mut signatures = 0;
        for constraint in interpreter.iter(secp, transaction, index, &Prevouts::All(prevouts)) {
            let pair = match constraint.map_err(|_| FinalizeError::InvalidWitness)? {
                SatisfiedConstraint::PublicKey { key_sig }
                | SatisfiedConstraint::PublicKeyHash { key_sig, .. } => Some(key_sig),
                _ => None,
            };
            if let Some(pair) = pair {
                match pair {
                    KeySigPair::Ecdsa(key, sig)
                        if sig.sighash_type == EcdsaSighashType::All
                            && original.inputs[index]
                                .bip32_derivation
                                .contains_key(&key.inner) =>
                    {
                        signatures += 1
                    }
                    _ => return Err(FinalizeError::InvalidWitness),
                }
            }
        }
        if signatures == 0 {
            return Err(FinalizeError::InvalidWitness);
        }
        counts.push(signatures);
    }
    Ok(counts)
}

#[cfg(test)]
mod tests;
