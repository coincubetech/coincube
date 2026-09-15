//! Explicit Bitcoin Blake2b P2WSH signing and signature verification.
//!
//! This API must be selected from an authenticated chain identity. A
//! [`bitcoin::Network`] value cannot distinguish Bitcoin from Bitcoin Blake2b,
//! because both chains use the same address and key encodings. Callers must also
//! obtain previous transactions from a trusted source for the selected chain:
//! matching a PSBT outpoint proves transaction linkage, not chain inclusion or
//! that an output is currently unspent.
//!
//! Verification here means that every supplied unified signature is valid for
//! the transaction, authenticated PSBT prevouts, and supported Vault witness
//! script. It does not establish a sufficient signature threshold, finalization,
//! authorization by a wallet policy, or replay protection. A valid PSBT with no
//! unified signatures returns `Ok(0)` from [`verify_p2wsh_all_unified`].

use std::{collections::BTreeSet, error, fmt};

use miniscript::{
    bitcoin::{
        hashes::Hash,
        psbt::{raw::ProprietaryKey, Psbt, PsbtSighashType},
        secp256k1, PublicKey, ScriptBuf, TxOut,
    },
    ExtParams, Miniscript, Segwitv0, Terminal,
};

use crate::{
    psbt_unified::{
        merge_signatures, unified_signatures, validate_internal, UnifiedPsbt, UnifiedPsbtError,
    },
    signer::MasterSigner,
    spend::{authenticate_previous_output, InputAuthError},
    unified_sighash::{UnifiedSighashCache, UnifiedSighashError, SCRIPT_TYPE_WITNESS_V0},
};

const UNIFIED_SIGHASH_ALL: u8 = 0x21;
const PROPRIETARY_PREFIX: &[u8] = b"coincube";
const PROPRIETARY_SUBTYPE: u8 = 0;

/// Failures from the explicit Bitcoin Blake2b P2WSH signing boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifiedSigningError {
    Adapter(UnifiedPsbtError),
    InputAuthentication {
        input: usize,
        reason: InputAuthError,
    },
    MissingWitnessScript {
        input: usize,
    },
    UnsupportedPrevoutScript {
        input: usize,
    },
    WitnessScriptCommitmentMismatch {
        input: usize,
    },
    UnsupportedWitnessScript {
        input: usize,
        reason: String,
    },
    IncompatibleSighash {
        input: usize,
        actual: u32,
    },
    DerivedPublicKeyMismatch {
        input: usize,
        public_key: PublicKey,
    },
    KeyNotInWitnessScript {
        input: usize,
        public_key: PublicKey,
    },
    InvalidUnifiedSignature {
        input: usize,
        public_key: PublicKey,
    },
    Sighash(UnifiedSighashError),
    PsbtConstruction(String),
}

impl fmt::Display for UnifiedSigningError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Adapter(err) => write!(f, "invalid unified PSBT: {err}"),
            Self::InputAuthentication { input, reason } => {
                write!(
                    f,
                    "input {input} previous output is not authenticated: {reason}"
                )
            }
            Self::MissingWitnessScript { input } => {
                write!(f, "input {input} is missing its witness script")
            }
            Self::UnsupportedPrevoutScript { input } => {
                write!(f, "input {input} does not spend native P2WSH")
            }
            Self::WitnessScriptCommitmentMismatch { input } => {
                write!(
                    f,
                    "input {input} witness script does not match its P2WSH output"
                )
            }
            Self::UnsupportedWitnessScript { input, reason } => {
                write!(
                    f,
                    "input {input} has an unsupported Vault witness script: {reason}"
                )
            }
            Self::IncompatibleSighash { input, actual } => write!(
                f,
                "input {input} requests incompatible sighash 0x{actual:08x}"
            ),
            Self::DerivedPublicKeyMismatch { input, public_key } => write!(
                f,
                "input {input} derivation does not produce public key {public_key}"
            ),
            Self::KeyNotInWitnessScript { input, public_key } => {
                write!(
                    f,
                    "public key {public_key} is not used by input {input}'s witness script"
                )
            }
            Self::InvalidUnifiedSignature { input, public_key } => write!(
                f,
                "unified signature for {public_key} in input {input} is cryptographically invalid"
            ),
            Self::Sighash(err) => write!(f, "unified sighash failed: {err}"),
            Self::PsbtConstruction(err) => write!(f, "could not construct signature delta: {err}"),
        }
    }
}

impl error::Error for UnifiedSigningError {}

impl From<UnifiedPsbtError> for UnifiedSigningError {
    fn from(value: UnifiedPsbtError) -> Self {
        Self::Adapter(value)
    }
}

impl From<UnifiedSighashError> for UnifiedSigningError {
    fn from(value: UnifiedSighashError) -> Self {
        Self::Sighash(value)
    }
}

struct InputContext {
    spent_output: TxOut,
    witness_script: ScriptBuf,
    miniscript: Miniscript<PublicKey, Segwitv0>,
}

/// Sign every supported input key derived from `signer` with ALL|UNIFIED.
///
/// The input is immutable. All inputs and all existing unified signatures are
/// validated before a local signature delta is created. The delta is applied
/// through the adapter's checked merge, and only inputs receiving a signature
/// have their PSBT sighash request set to raw `0x21`.
pub fn sign_p2wsh_all_unified(
    signer: &MasterSigner,
    psbt: &UnifiedPsbt,
    secp: &secp256k1::Secp256k1<secp256k1::All>,
) -> Result<UnifiedPsbt, UnifiedSigningError> {
    let contexts = validate_inputs(psbt)?;
    verify_with_contexts(psbt, secp, &contexts)?;

    let fingerprint = signer.fingerprint(secp);
    let mut signatures = Vec::new();
    for (input_index, input) in psbt.psbt().inputs.iter().enumerate() {
        let context = &contexts[input_index];
        for (raw_public_key, (origin, path)) in &input.bip32_derivation {
            if *origin != fingerprint {
                continue;
            }
            let public_key = PublicKey::new(*raw_public_key);
            let derived = signer.xpriv_at(path, secp).to_priv().public_key(secp);
            if derived != public_key {
                return Err(UnifiedSigningError::DerivedPublicKeyMismatch {
                    input: input_index,
                    public_key,
                });
            }
            if !script_uses_key(&context.miniscript, &public_key) {
                return Err(UnifiedSigningError::KeyNotInWitnessScript {
                    input: input_index,
                    public_key,
                });
            }
            require_compatible_sighash(input_index, input.sighash_type)?;
            signatures.push((input_index, public_key, path.clone()));
        }
    }

    if signatures.is_empty() {
        return Ok(psbt.clone());
    }

    let spent_outputs: Vec<_> = contexts
        .iter()
        .map(|ctx| ctx.spent_output.clone())
        .collect();
    let cache = UnifiedSighashCache::new(&psbt.psbt().unsigned_tx, &spent_outputs)?;
    let mut delta = empty_signature_delta(psbt)?;
    let mut signed_inputs = BTreeSet::new();
    for (input_index, public_key, path) in signatures {
        let digest = cache.signature_hash(
            input_index,
            UNIFIED_SIGHASH_ALL,
            SCRIPT_TYPE_WITNESS_V0,
            &contexts[input_index].witness_script,
        )?;
        let message = secp256k1::Message::from_digest(digest);
        let private_key = signer.xpriv_at(&path, secp).to_priv();
        let signature = secp.sign_ecdsa_low_r(&message, &private_key.inner);
        let mut encoded = signature.serialize_der().to_vec();
        encoded.push(UNIFIED_SIGHASH_ALL);
        delta.psbt_mut().inputs[input_index]
            .proprietary
            .insert(proprietary_key(&public_key), encoded);
        signed_inputs.insert(input_index);
    }
    validate_internal(&delta)?;

    let mut result = psbt.clone();
    merge_signatures(&mut result, &delta)?;
    for input_index in signed_inputs {
        result.psbt_mut().inputs[input_index].sighash_type =
            Some(PsbtSighashType::from_u32(u32::from(UNIFIED_SIGHASH_ALL)));
    }
    validate_internal(&result)?;
    verify_p2wsh_all_unified(&result, secp)?;
    Ok(result)
}

/// Verify every supplied unified record and return the number verified.
///
/// `Ok(0)` explicitly means that the PSBT and its supported P2WSH inputs were
/// validated but it contained no unified signatures. It does not mean the PSBT
/// is sufficiently signed or finalizable.
pub fn verify_p2wsh_all_unified<C: secp256k1::Verification>(
    psbt: &UnifiedPsbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<usize, UnifiedSigningError> {
    let contexts = validate_inputs(psbt)?;
    verify_with_contexts(psbt, secp, &contexts)
}

fn validate_inputs(psbt: &UnifiedPsbt) -> Result<Vec<InputContext>, UnifiedSigningError> {
    validate_internal(psbt)?;
    let mut contexts = Vec::with_capacity(psbt.psbt().inputs.len());
    for (input_index, (txin, input)) in psbt
        .psbt()
        .unsigned_tx
        .input
        .iter()
        .zip(&psbt.psbt().inputs)
        .enumerate()
    {
        let spent_output = authenticate_previous_output(
            &txin.previous_output,
            input.non_witness_utxo.as_ref(),
            input.witness_utxo.as_ref(),
        )
        .map_err(|reason| UnifiedSigningError::InputAuthentication {
            input: input_index,
            reason,
        })?;
        if !spent_output.script_pubkey.is_p2wsh() {
            return Err(UnifiedSigningError::UnsupportedPrevoutScript { input: input_index });
        }
        let witness_script = input
            .witness_script
            .clone()
            .ok_or(UnifiedSigningError::MissingWitnessScript { input: input_index })?;
        if witness_script.to_p2wsh() != spent_output.script_pubkey {
            return Err(UnifiedSigningError::WitnessScriptCommitmentMismatch {
                input: input_index,
            });
        }
        // Decoding a compiled `pkh()` leaf necessarily recovers only its hash,
        // so permit that one representation gap while retaining every other
        // Miniscript sanity rule.
        let miniscript = Miniscript::<PublicKey, Segwitv0>::parse_with_ext(
            &witness_script,
            &ExtParams::sane().raw_pkh(),
        )
        .map_err(|err| UnifiedSigningError::UnsupportedWitnessScript {
            input: input_index,
            reason: err.to_string(),
        })?;
        contexts.push(InputContext {
            spent_output,
            witness_script,
            miniscript,
        });
    }
    Ok(contexts)
}

fn verify_with_contexts<C: secp256k1::Verification>(
    psbt: &UnifiedPsbt,
    secp: &secp256k1::Secp256k1<C>,
    contexts: &[InputContext],
) -> Result<usize, UnifiedSigningError> {
    let signatures = unified_signatures(psbt)?;
    for signature in &signatures {
        require_compatible_sighash(
            signature.input_index,
            psbt.psbt().inputs[signature.input_index].sighash_type,
        )?;
        if !script_uses_key(
            &contexts[signature.input_index].miniscript,
            &signature.public_key,
        ) {
            return Err(UnifiedSigningError::KeyNotInWitnessScript {
                input: signature.input_index,
                public_key: signature.public_key,
            });
        }
    }

    let spent_outputs: Vec<_> = contexts
        .iter()
        .map(|ctx| ctx.spent_output.clone())
        .collect();
    let cache = UnifiedSighashCache::new(&psbt.psbt().unsigned_tx, &spent_outputs)?;
    for record in &signatures {
        let digest = cache.signature_hash(
            record.input_index,
            UNIFIED_SIGHASH_ALL,
            SCRIPT_TYPE_WITNESS_V0,
            &contexts[record.input_index].witness_script,
        )?;
        let signature =
            secp256k1::ecdsa::Signature::from_der(&record.signature[..record.signature.len() - 1])
                .map_err(|_| UnifiedSigningError::InvalidUnifiedSignature {
                    input: record.input_index,
                    public_key: record.public_key,
                })?;
        let message = secp256k1::Message::from_digest(digest);
        secp.verify_ecdsa(&message, &signature, &record.public_key.inner)
            .map_err(|_| UnifiedSigningError::InvalidUnifiedSignature {
                input: record.input_index,
                public_key: record.public_key,
            })?;
    }
    Ok(signatures.len())
}

fn require_compatible_sighash(
    input: usize,
    requested: Option<PsbtSighashType>,
) -> Result<(), UnifiedSigningError> {
    if let Some(requested) = requested {
        let actual = requested.to_u32();
        if actual != u32::from(UNIFIED_SIGHASH_ALL) {
            return Err(UnifiedSigningError::IncompatibleSighash { input, actual });
        }
    }
    Ok(())
}

fn script_uses_key(miniscript: &Miniscript<PublicKey, Segwitv0>, public_key: &PublicKey) -> bool {
    miniscript.iter().any(|node| match &node.node {
        Terminal::PkK(key) | Terminal::PkH(key) => key == public_key,
        Terminal::RawPkH(hash) => public_key.pubkey_hash().as_byte_array() == hash.as_byte_array(),
        Terminal::Multi(keys) => keys.iter().any(|key| key == public_key),
        _ => false,
    })
}

fn proprietary_key(public_key: &PublicKey) -> ProprietaryKey {
    ProprietaryKey {
        prefix: PROPRIETARY_PREFIX.to_vec(),
        subtype: PROPRIETARY_SUBTYPE,
        key: public_key.to_bytes(),
    }
}

fn empty_signature_delta(psbt: &UnifiedPsbt) -> Result<UnifiedPsbt, UnifiedSigningError> {
    let mut raw = Psbt::from_unsigned_tx(psbt.psbt().unsigned_tx.clone())
        .map_err(|err| UnifiedSigningError::PsbtConstruction(err.to_string()))?;
    for (destination, source) in raw.inputs.iter_mut().zip(&psbt.psbt().inputs) {
        destination.non_witness_utxo = source.non_witness_utxo.clone();
        destination.witness_utxo = source.witness_utxo.clone();
        destination.witness_script = source.witness_script.clone();
        destination.bip32_derivation = source.bip32_derivation.clone();
    }
    UnifiedPsbt::from_psbt(raw).map_err(Into::into)
}

#[cfg(test)]
mod tests;
