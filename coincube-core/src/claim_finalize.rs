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
    finalize_transfer(
        construction.psbt(),
        construction.descriptor(),
        construction.chain(),
        signed,
        secp,
    )
}

fn finalize_transfer<C: secp256k1::Verification>(
    original: &Psbt,
    descriptor: &CoincubeDescriptor,
    chain: ChainId,
    signed: &Psbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedPoisonTransfer, FinalizeError> {
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
    spend::reverify_spend_before_broadcast(descriptor, signed)
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
        chain,
        descriptor: descriptor.clone(),
        fee,
        signatures_per_input,
    })
}

/// A finalized fork sweep and its verified retained-witness reports. This is
/// cryptographic evidence, not proof that Bitcoin's poison transfer is confirmed
/// or permission to broadcast a legacy-signed sweep.
#[derive(Debug)]
pub struct VerifiedClaimForkSweep {
    finalized: crate::unified_finalize::FinalizedSpend,
    chain: ChainId,
    bitcoin_step1: Txid,
    descriptor: CoincubeDescriptor,
    fee: Amount,
}
impl VerifiedClaimForkSweep {
    pub fn transaction(&self) -> &Transaction {
        &self.finalized.transaction
    }
    pub fn inputs(&self) -> &[crate::unified_finalize::InputWitnessReport] {
        &self.finalized.inputs
    }
    pub fn chain(&self) -> ChainId {
        self.chain
    }
    pub fn bitcoin_step1(&self) -> Txid {
        self.bitcoin_step1
    }
    pub fn descriptor(&self) -> &CoincubeDescriptor {
        &self.descriptor
    }
    pub fn fee(&self) -> Amount {
        self.fee
    }
}

#[derive(Debug)]
pub enum ClaimForkFinalizeError {
    ConstructionChanged,
    InvalidRecoveredWitness,
    Adapter(crate::psbt_unified::UnifiedPsbtError),
    Finalize(crate::unified_finalize::UnifiedFinalizeError),
    Economics,
}
impl std::fmt::Display for ClaimForkFinalizeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ConstructionChanged => f.write_str("Claim fork sweep construction changed"),
            Self::InvalidRecoveredWitness => {
                f.write_str("Recovered Claim fork witness is invalid or noncanonical")
            }
            Self::Adapter(e) => e.fmt(f),
            Self::Finalize(e) => e.fmt(f),
            Self::Economics => f.write_str("Claim fork sweep economics are invalid"),
        }
    }
}
impl std::error::Error for ClaimForkFinalizeError {}

/// Validate an in-progress signing PSBT against its owned fork construction.
/// Incomplete signature sets are permitted here; metadata, amounts, scripts and
/// outputs are not replaceable. This is not signature or replay-safety evidence.
pub fn validate_claim_fork_signing(
    construction: &crate::claim_spend::ClaimForkSweep,
    signed: &crate::psbt_unified::UnifiedPsbt,
) -> Result<(), ClaimForkFinalizeError> {
    use crate::psbt_unified::{merge_signatures, UnifiedPsbt};
    let mut expected = UnifiedPsbt::from_psbt(construction.psbt().clone())
        .map_err(ClaimForkFinalizeError::Adapter)?;
    merge_signatures(&mut expected, signed).map_err(ClaimForkFinalizeError::Adapter)?;
    // The adapter validates permitted sighash requests. Copy only this signing
    // field in addition to the signature records, then compare the entire PSBT.
    for (original, supplied) in expected
        .psbt_mut()
        .inputs
        .iter_mut()
        .zip(&signed.psbt().inputs)
    {
        original.sighash_type = supplied.sighash_type;
    }
    if expected.psbt() != signed.psbt() {
        return Err(ClaimForkFinalizeError::ConstructionChanged);
    }
    spend::reverify_spend_before_broadcast(construction.descriptor(), signed.psbt())
        .map_err(|_| ClaimForkFinalizeError::Economics)?;
    Ok(())
}

/// Only validated signing additions may differ from the owned construction.
/// Full prevouts, key origins, scripts, output metadata and unrelated maps must
/// remain exact; prefinalized/imported replacements do not bypass the verifier.
/// Both unified and legacy signatures are cryptographically checked by the
/// existing finalizer. A legacy-only report still requires fresh poison proof
/// from the coordinator before any replay-safety claim or submission.
///
/// The PSBT is also refused when an input could be finalised with a unified
/// signature while the legacy signatures it retains independently satisfy the
/// script ([`ensure_no_unsafe_legacy_alternative`], `#582`). This is the only
/// constructor of a [`VerifiedClaimForkSweep`] that reads a signing PSBT, so
/// the Claim submission route, which receives only the finalised artifact,
/// never sees such a PSBT.
///
/// [`ensure_no_unsafe_legacy_alternative`]: crate::unified_finalize::ensure_no_unsafe_legacy_alternative
pub fn finalize_claim_fork_sweep<C: secp256k1::Verification>(
    construction: &crate::claim_spend::ClaimForkSweep,
    signed: &crate::psbt_unified::UnifiedPsbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedClaimForkSweep, ClaimForkFinalizeError> {
    let verified = finalize_owned_claim_fork_sweep(construction, signed, secp)?;
    crate::unified_finalize::ensure_no_unsafe_legacy_alternative(signed, secp)
        .map_err(ClaimForkFinalizeError::Finalize)?;
    Ok(verified)
}

/// Construction binding and cryptographic finalisation, without the retention
/// refusal. Recovery uses it directly: its PSBT is rebuilt from a witness that
/// was already published, holds only the signatures that witness carries, and
/// is never stored or exported. Refusing it could not withdraw the published
/// bytes; it would only stop the wallet tracking them.
fn finalize_owned_claim_fork_sweep<C: secp256k1::Verification>(
    construction: &crate::claim_spend::ClaimForkSweep,
    signed: &crate::psbt_unified::UnifiedPsbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedClaimForkSweep, ClaimForkFinalizeError> {
    validate_claim_fork_signing(construction, signed)?;
    let fee = signed
        .psbt()
        .fee()
        .map_err(|_| ClaimForkFinalizeError::Economics)?;
    let finalized = crate::unified_finalize::finalize_p2wsh_all_unified(signed, secp)
        .map_err(ClaimForkFinalizeError::Finalize)?;
    Ok(VerifiedClaimForkSweep {
        finalized,
        chain: construction.chain(),
        bitcoin_step1: construction.bitcoin_step1(),
        descriptor: construction.descriptor().clone(),
        fee,
    })
}

/// Recover the exact canonical witness produced by this Claim finalizer.
/// Signatures are checked against owned prevouts and keys, then fed through the
/// existing Miniscript finalizer. Its output must equal the recovered bytes,
/// including every witness item. No journal value or matching txid substitutes
/// for cryptographic verification, and this grants no permission to resubmit.
pub fn verify_claim_fork_transaction<C: secp256k1::Verification>(
    construction: &crate::claim_spend::ClaimForkSweep,
    transaction: &Transaction,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedClaimForkSweep, ClaimForkFinalizeError> {
    use crate::{
        psbt_unified::{proprietary_key, UnifiedPsbt},
        unified_sighash::{UnifiedSighashCache, SCRIPT_TYPE_WITNESS_V0},
    };
    use bitcoin::{ecdsa, PublicKey};
    let invalid = || ClaimForkFinalizeError::InvalidRecoveredWitness;
    let original = construction.psbt();
    let mut unsigned = transaction.clone();
    for input in &mut unsigned.input {
        input.witness.clear();
    }
    if unsigned != original.unsigned_tx {
        return Err(ClaimForkFinalizeError::ConstructionChanged);
    }
    spend::reverify_spend_before_broadcast(construction.descriptor(), original)
        .map_err(|_| ClaimForkFinalizeError::Economics)?;
    let prevouts = original
        .inputs
        .iter()
        .zip(&original.unsigned_tx.input)
        .map(|(input, txin)| {
            spend::authenticate_previous_output(
                &txin.previous_output,
                input.non_witness_utxo.as_ref(),
                input.witness_utxo.as_ref(),
            )
            .map_err(|_| invalid())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let unified =
        UnifiedSighashCache::new(&original.unsigned_tx, &prevouts).map_err(|_| invalid())?;
    let mut legacy = SighashCache::new(&original.unsigned_tx);
    let mut recovered =
        UnifiedPsbt::from_psbt(original.clone()).map_err(ClaimForkFinalizeError::Adapter)?;
    for (index, input) in original.inputs.iter().enumerate() {
        let script = input.witness_script.as_ref().ok_or_else(invalid)?;
        let witness = &transaction.input[index].witness;
        if witness.last() != Some(script.as_bytes()) {
            return Err(invalid());
        }
        let unified_message = secp256k1::Message::from_digest(
            unified
                .signature_hash(index, 0x21, SCRIPT_TYPE_WITNESS_V0, script)
                .map_err(|_| invalid())?,
        );
        let legacy_message = secp256k1::Message::from_digest(
            legacy
                .p2wsh_signature_hash(index, script, prevouts[index].value, EcdsaSighashType::All)
                .map_err(|_| invalid())?
                .to_byte_array(),
        );
        for item in witness.iter().take(witness.len().saturating_sub(1)) {
            let Some((&tag, der)) = item.split_last() else {
                continue;
            };
            let Ok(signature) = secp256k1::ecdsa::Signature::from_der(der) else {
                continue;
            };
            let message = match tag {
                1 => &legacy_message,
                0x21 => &unified_message,
                _ => return Err(invalid()),
            };
            let key = input
                .bip32_derivation
                .keys()
                .find(|key| secp.verify_ecdsa(message, &signature, key).is_ok())
                .ok_or_else(invalid)?;
            let key = PublicKey::new(*key);
            let dest = &mut recovered.psbt_mut().inputs[index];
            if tag == 0x21 {
                dest.proprietary
                    .insert(proprietary_key(&key), item.to_vec());
            } else {
                dest.partial_sigs.insert(
                    key,
                    ecdsa::Signature {
                        signature,
                        sighash_type: EcdsaSighashType::All,
                    },
                );
            }
        }
    }
    let verified = finalize_owned_claim_fork_sweep(construction, &recovered, secp)?;
    if verified.transaction() != transaction {
        return Err(invalid());
    }
    // The retention refusal is skipped here by design, and it also cannot fire
    // (`#607`). A witness carries one satisfaction, so next to a unified
    // signature a k-of-n branch holds at most k-1 legacy signatures, and those
    // cannot satisfy it alone. Only a key shared across spending paths could
    // break that; debug builds check it so the skip is revisited if it does.
    debug_assert!(
        crate::unified_finalize::ensure_no_unsafe_legacy_alternative(&recovered, secp).is_ok(),
        "a recovered Claim fork witness retained a legacy alternative: the k-1 invariant broke"
    );
    Ok(verified)
}

/// Verify a transaction recovered from the node against a reconstructed owned
/// poison transfer. Disk state and a matching txid are not signature evidence:
/// every retained witness is interpreted against authenticated previous outputs.
/// The result still grants no broadcast, replay-safety or step-two permission.
pub fn verify_poison_transaction<C: secp256k1::Verification>(
    construction: &PoisonSelfTransfer,
    transaction: &Transaction,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedPoisonTransfer, FinalizeError> {
    verify_transfer(
        construction.psbt(),
        construction.descriptor(),
        construction.chain(),
        transaction,
        secp,
    )
}

fn verify_transfer<C: secp256k1::Verification>(
    original: &Psbt,
    descriptor: &CoincubeDescriptor,
    chain: ChainId,
    transaction: &Transaction,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<VerifiedPoisonTransfer, FinalizeError> {
    spend::reverify_spend_before_broadcast(descriptor, original)
        .map_err(|_| FinalizeError::Economics)?;
    let prevouts = original
        .inputs
        .iter()
        .zip(&original.unsigned_tx.input)
        .map(|(input, txin)| {
            spend::authenticate_previous_output(
                &txin.previous_output,
                input.non_witness_utxo.as_ref(),
                input.witness_utxo.as_ref(),
            )
            .map_err(|_| FinalizeError::InputAuthentication)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let signatures_per_input = verify_retained_witness(transaction, original, &prevouts, secp)?;
    Ok(VerifiedPoisonTransfer {
        transaction: transaction.clone(),
        construction_txid: original.unsigned_tx.compute_txid(),
        chain,
        descriptor: descriptor.clone(),
        fee: original.fee().map_err(|_| FinalizeError::Economics)?,
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

mod ancestry;
pub use ancestry::{
    finalize_ancestry_transfer, verify_ancestry_transaction, VerifiedAncestryTransfer,
};

#[cfg(test)]
mod tests;
