//! Bitcoin Blake2b unified signing, verification and finalisation for
//! *foreign* wallet inputs: the script types the Split tool's unified-sweep
//! fallback spends from a non-Cube wallet (desktop plan PR 8).
//!
//! Supported, per authenticated previous output:
//!
//! | Input            | Unified script type | scriptCode                   |
//! |------------------|---------------------|------------------------------|
//! | P2WPKH           | 1 (segwit v0)       | implied P2PKH script (BIP143) |
//! | P2SH-P2WPKH      | 1 (segwit v0)       | implied P2PKH script (BIP143) |
//! | P2PKH            | 0 (bare/P2SH)       | the scriptPubKey             |
//! | P2WSH `multi`    | 1 (segwit v0)       | the witnessScript            |
//!
//! Script type and scriptCode follow Bitcoin Knots `v29.4.1.knots20260508`
//! (`doc/unified-sighash.md`, "scriptCode is what the legacy rules already
//! use"; `SignatureHashUnified` maps `SigVersion::BASE` to 0 and
//! `WITNESS_V0` to 1). `sortedmulti` compiles to the same `multi` script with
//! sorted keys, so it is covered by the `multi` row. The digest itself is
//! [`UnifiedSighashCache`], checked against the upstream `unified_sighash.json`
//! vectors; those vectors use synthetic scripts, so the per-type choices in
//! the table are *not* upstream known answers (see the tests module).
//!
//! Everything else is refused, never guessed: Taproot outputs (`tr` is
//! scan-only in Split), `sh(wsh(..))`, bare scripts, P2WSH scripts other than
//! a top-level `multi` (Vault inputs use [`crate::unified_signing`]), inputs
//! whose PSBT data does not commit to the output, already-finalised inputs,
//! and every input carrying a legacy `partial_sigs` entry. The last one keeps
//! this path unified-only: a replay-protection report can never be built from
//! a legacy witness, and no retained legacy set can form an alternative
//! Bitcoin-valid witness (`#398`, `#536`). `ANYONECANPAY` and every sighash
//! request other than absent or `ALL|UNIFIED` are refused by the adapter and
//! again here.
//!
//! Signatures are stored exactly as the P2WSH Vault path stores them: in the
//! `coincube` proprietary namespace of [`crate::psbt_unified`], `DER || 0x21`,
//! with the input's `PSBT_IN_SIGHASH_TYPE` moved to `0x21` on inputs this
//! signer signed.

use std::{collections::BTreeSet, convert::TryFrom, error, fmt};

use miniscript::{
    bitcoin::{
        hashes::Hash,
        psbt::{self, raw::ProprietaryKey, Psbt, PsbtSighashType},
        script::{Builder, PushBytesBuf},
        secp256k1, PubkeyHash, PublicKey, ScriptBuf, TxOut, Witness,
    },
    ExtParams, Miniscript, Segwitv0, Terminal,
};

use crate::{
    chain::ChainId,
    psbt_unified::{
        merge_signatures, unified_signatures, validate_internal, UnifiedPsbt, UnifiedPsbtError,
        UnifiedSignature,
    },
    signer::SessionSigner,
    spend::{authenticate_previous_output, InputAuthError},
    unified_finalize::{FinalizedSpend, InputWitnessReport},
    unified_sighash::{
        UnifiedSighashCache, UnifiedSighashError, SCRIPT_TYPE_BASE, SCRIPT_TYPE_WITNESS_V0,
    },
};

const UNIFIED_SIGHASH_ALL: u8 = 0x21;
const PROPRIETARY_PREFIX: &[u8] = b"coincube";
const PROPRIETARY_SUBTYPE: u8 = 0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForeignUnifiedError {
    /// Unified signing was asked for on a chain that is not Bitcoin Blake2b.
    /// A `0x21` signature is invalid on Bitcoin (invariant I5).
    NotBitcoinBlake2b(ChainId),
    Adapter(UnifiedPsbtError),
    Sighash(UnifiedSighashError),
    InputAuthentication {
        input: usize,
        reason: InputAuthError,
    },
    /// Taproot is scan-only in Split: no unified Taproot signing exists here.
    TaprootScanOnly {
        input: usize,
    },
    /// Not one of the four supported shapes, or PSBT data that does not
    /// match it (missing/extra/uncommitted scripts, Taproot fields, already
    /// finalised).
    UnsupportedScript {
        input: usize,
        reason: String,
    },
    /// A legacy `partial_sigs` entry. This path is unified-only.
    LegacySignature {
        input: usize,
    },
    IncompatibleSighash {
        input: usize,
        actual: u32,
    },
    DerivedPublicKeyMismatch {
        input: usize,
        public_key: PublicKey,
    },
    KeyNotInScript {
        input: usize,
        public_key: PublicKey,
    },
    InvalidUnifiedSignature {
        input: usize,
        public_key: PublicKey,
    },
    /// The session signer controls no key in any input.
    NothingToSign,
    /// Too few verified unified signatures to satisfy the input.
    Unsatisfiable {
        input: usize,
        have: usize,
        need: usize,
    },
}

impl fmt::Display for ForeignUnifiedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotBitcoinBlake2b(chain) => write!(
                f,
                "unified signing is only produced on Bitcoin Blake2b, not {chain:?}"
            ),
            Self::Adapter(err) => write!(f, "invalid unified PSBT: {err}"),
            Self::Sighash(err) => write!(f, "unified sighash failed: {err}"),
            Self::InputAuthentication { input, reason } => write!(
                f,
                "input {input} previous output is not authenticated: {reason}"
            ),
            Self::TaprootScanOnly { input } => write!(
                f,
                "input {input} spends Taproot, which Split can scan but not unified-sign"
            ),
            Self::UnsupportedScript { input, reason } => {
                write!(f, "input {input} has an unsupported script: {reason}")
            }
            Self::LegacySignature { input } => write!(
                f,
                "input {input} carries a legacy signature; the foreign unified path is unified-only"
            ),
            Self::IncompatibleSighash { input, actual } => write!(
                f,
                "input {input} requests incompatible sighash 0x{actual:08x}"
            ),
            Self::DerivedPublicKeyMismatch { input, public_key } => write!(
                f,
                "input {input} derivation does not produce public key {public_key}"
            ),
            Self::KeyNotInScript { input, public_key } => {
                write!(f, "public key {public_key} is not used by input {input}")
            }
            Self::InvalidUnifiedSignature { input, public_key } => write!(
                f,
                "unified signature for {public_key} in input {input} is invalid"
            ),
            Self::NothingToSign => write!(f, "the session signer controls no key in this PSBT"),
            Self::Unsatisfiable { input, have, need } => write!(
                f,
                "input {input} has {have} unified signatures, needs {need}"
            ),
        }
    }
}

impl error::Error for ForeignUnifiedError {}

impl From<UnifiedPsbtError> for ForeignUnifiedError {
    fn from(value: UnifiedPsbtError) -> Self {
        Self::Adapter(value)
    }
}

impl From<UnifiedSighashError> for ForeignUnifiedError {
    fn from(value: UnifiedSighashError) -> Self {
        Self::Sighash(value)
    }
}

enum Keys {
    /// Single-sig: HASH160 of the one key. Segwit requires it compressed.
    Hash {
        hash: [u8; 20],
        compressed_only: bool,
    },
    Multi {
        threshold: usize,
        keys: Vec<PublicKey>,
    },
}

struct InputContext {
    spent_output: TxOut,
    script_code: ScriptBuf,
    script_type: u8,
    keys: Keys,
    /// P2SH-P2WPKH redeem script or P2WSH witness script, for finalisation.
    script: Option<ScriptBuf>,
}

impl InputContext {
    fn uses_key(&self, public_key: &PublicKey) -> bool {
        match &self.keys {
            Keys::Hash {
                hash,
                compressed_only,
            } => {
                (public_key.compressed || !compressed_only)
                    && public_key.pubkey_hash().as_byte_array() == hash
            }
            Keys::Multi { keys, .. } => keys.contains(public_key),
        }
    }
}

/// Sign every input key `signer` controls with `ALL|UNIFIED`. Reached through
/// [`SessionSigner::sign_unified`].
pub(crate) fn sign_foreign_unified<C: secp256k1::Signing + secp256k1::Verification>(
    signer: &SessionSigner,
    psbt: &UnifiedPsbt,
    chain: ChainId,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<UnifiedPsbt, ForeignUnifiedError> {
    // I5: the selection is a pure function of the chain, checked first.
    if !chain.is_blake2b() {
        return Err(ForeignUnifiedError::NotBitcoinBlake2b(chain));
    }
    let contexts = validate_inputs(psbt)?;
    verify_with_contexts(psbt, secp, &contexts)?;

    let fingerprint = signer.fingerprint(secp);
    let mut wanted = Vec::new();
    for (input_index, input) in psbt.psbt().inputs.iter().enumerate() {
        for (raw_public_key, (origin, path)) in &input.bip32_derivation {
            if *origin != fingerprint {
                continue;
            }
            let public_key = PublicKey::new(*raw_public_key);
            if signer.public_key_at(path, secp).ok() != Some(*raw_public_key) {
                return Err(ForeignUnifiedError::DerivedPublicKeyMismatch {
                    input: input_index,
                    public_key,
                });
            }
            if !contexts[input_index].uses_key(&public_key) {
                return Err(ForeignUnifiedError::KeyNotInScript {
                    input: input_index,
                    public_key,
                });
            }
            require_compatible_sighash(input_index, input.sighash_type)?;
            wanted.push((input_index, public_key, path.clone()));
        }
    }
    if wanted.is_empty() {
        return Err(ForeignUnifiedError::NothingToSign);
    }

    let spent: Vec<_> = contexts.iter().map(|c| c.spent_output.clone()).collect();
    let cache = UnifiedSighashCache::new(&psbt.psbt().unsigned_tx, &spent)?;
    let mut delta = Psbt::from_unsigned_tx(psbt.psbt().unsigned_tx.clone())
        .expect("validated unsigned transaction has no scriptSig or witness");
    let mut signed_inputs = BTreeSet::new();
    for (input_index, public_key, path) in wanted {
        let context = &contexts[input_index];
        let digest = cache.signature_hash(
            input_index,
            UNIFIED_SIGHASH_ALL,
            context.script_type,
            &context.script_code,
        )?;
        let signature = signer
            .sign_digest_at(&path, digest, secp)
            .expect("path derived above");
        let mut encoded = signature.serialize_der().to_vec();
        encoded.push(UNIFIED_SIGHASH_ALL);
        delta.inputs[input_index]
            .proprietary
            .insert(proprietary_key(&public_key), encoded);
        signed_inputs.insert(input_index);
    }
    let delta = UnifiedPsbt::from_psbt(delta)?;

    let mut result = psbt.clone();
    merge_signatures(&mut result, &delta)?;
    for input_index in signed_inputs {
        result.psbt_mut().inputs[input_index].sighash_type =
            Some(PsbtSighashType::from_u32(u32::from(UNIFIED_SIGHASH_ALL)));
    }
    validate_internal(&result)?;
    verify_foreign_unified(&result, secp)?;
    Ok(result)
}

/// Verify every unified record against its input's authenticated prevouts,
/// script type and scriptCode, and return how many were verified. `Ok(0)`
/// means a valid, unsigned PSBT; it says nothing about sufficiency.
pub fn verify_foreign_unified<C: secp256k1::Verification>(
    psbt: &UnifiedPsbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<usize, ForeignUnifiedError> {
    let contexts = validate_inputs(psbt)?;
    verify_with_contexts(psbt, secp, &contexts)
}

/// Finalise every input from verified unified signatures only, returning the
/// transaction and, per input, the witness composition. Every input of a
/// successful result is `replay_protected()`: this path never places a legacy
/// signature, and an input it cannot satisfy from unified signatures is
/// refused rather than reported replayable.
pub fn finalize_foreign_unified<C: secp256k1::Verification>(
    psbt: &UnifiedPsbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<FinalizedSpend, ForeignUnifiedError> {
    let contexts = validate_inputs(psbt)?;
    verify_with_contexts(psbt, secp, &contexts)?;
    let records = unified_signatures(psbt)?;
    let mut transaction = psbt.psbt().unsigned_tx.clone();
    let mut reports = Vec::with_capacity(contexts.len());

    for (input_index, context) in contexts.iter().enumerate() {
        let mut mine = records.iter().filter(|r| r.input_index == input_index);
        let txin = &mut transaction.input[input_index];
        let used = match &context.keys {
            Keys::Hash { .. } => {
                // Verified records all hash to the one key, so at most one.
                let record = mine.next().ok_or(ForeignUnifiedError::Unsatisfiable {
                    input: input_index,
                    have: 0,
                    need: 1,
                })?;
                let key = record.public_key.to_bytes();
                if context.script_type == SCRIPT_TYPE_BASE {
                    txin.script_sig = Builder::new()
                        .push_slice(push_bytes(record.signature.clone()))
                        .push_slice(push_bytes(key))
                        .into_script();
                } else {
                    if let Some(redeem) = &context.script {
                        txin.script_sig = Builder::new()
                            .push_slice(push_bytes(redeem.to_bytes()))
                            .into_script();
                    }
                    txin.witness = Witness::from_slice(&[record.signature.clone(), key]);
                }
                1
            }
            Keys::Multi { threshold, keys } => {
                // CHECKMULTISIG consumes signatures in key order: take the
                // first `threshold` signed keys in script order.
                let mut elements: Vec<Vec<u8>> = vec![Vec::new()];
                for key in keys {
                    if elements.len() > *threshold {
                        break;
                    }
                    if let Some(record) = records
                        .iter()
                        .find(|r| r.input_index == input_index && r.public_key == *key)
                    {
                        elements.push(record.signature.clone());
                    }
                }
                let have = elements.len() - 1;
                if have < *threshold {
                    return Err(ForeignUnifiedError::Unsatisfiable {
                        input: input_index,
                        have,
                        need: *threshold,
                    });
                }
                let script = context.script.as_ref().expect("P2WSH context has a script");
                elements.push(script.to_bytes());
                txin.witness = Witness::from_slice(&elements);
                have
            }
        };
        reports.push(InputWitnessReport {
            unified_used: used,
            legacy_used: 0,
        });
    }
    Ok(FinalizedSpend {
        transaction,
        inputs: reports,
    })
}

fn push_bytes(bytes: Vec<u8>) -> PushBytesBuf {
    PushBytesBuf::try_from(bytes).expect("signatures, keys and redeem scripts are short pushes")
}

fn validate_inputs(psbt: &UnifiedPsbt) -> Result<Vec<InputContext>, ForeignUnifiedError> {
    validate_internal(psbt)?;
    let raw = psbt.psbt();
    let mut contexts = Vec::with_capacity(raw.inputs.len());
    for (input_index, (txin, input)) in raw.unsigned_tx.input.iter().zip(&raw.inputs).enumerate() {
        let spent_output = authenticate_previous_output(
            &txin.previous_output,
            input.non_witness_utxo.as_ref(),
            input.witness_utxo.as_ref(),
        )
        .map_err(|reason| ForeignUnifiedError::InputAuthentication {
            input: input_index,
            reason,
        })?;
        contexts.push(classify(input_index, spent_output, input)?);
    }
    Ok(contexts)
}

fn classify(
    input_index: usize,
    spent_output: TxOut,
    input: &psbt::Input,
) -> Result<InputContext, ForeignUnifiedError> {
    let spk = spent_output.script_pubkey.clone();
    let unsupported = |reason: &str| ForeignUnifiedError::UnsupportedScript {
        input: input_index,
        reason: reason.to_string(),
    };
    if spk.is_p2tr() {
        return Err(ForeignUnifiedError::TaprootScanOnly { input: input_index });
    }
    let tap_fields = input.tap_key_sig.is_some()
        || !input.tap_script_sigs.is_empty()
        || !input.tap_scripts.is_empty()
        || !input.tap_key_origins.is_empty()
        || input.tap_internal_key.is_some()
        || input.tap_merkle_root.is_some();
    if tap_fields {
        return Err(unsupported("non-Taproot input carries Taproot fields"));
    }
    if input.final_script_sig.is_some() || input.final_script_witness.is_some() {
        return Err(unsupported("input is already finalised"));
    }
    if !input.partial_sigs.is_empty() {
        return Err(ForeignUnifiedError::LegacySignature { input: input_index });
    }

    // Single key: scriptCode is the P2PKH script of the key hash, which for a
    // P2PKH output is the scriptPubKey itself (script type 0).
    let single = |program: &[u8], script_type, script: Option<ScriptBuf>| {
        let mut hash = [0u8; 20];
        hash.copy_from_slice(program);
        InputContext {
            spent_output: spent_output.clone(),
            script_code: ScriptBuf::new_p2pkh(&PubkeyHash::from_byte_array(hash)),
            script_type,
            keys: Keys::Hash {
                hash,
                compressed_only: script_type != SCRIPT_TYPE_BASE,
            },
            script,
        }
    };
    let scripts = (input.redeem_script.clone(), input.witness_script.clone());

    if spk.is_p2wpkh() || spk.is_p2pkh() {
        if scripts != (None, None) {
            return Err(unsupported(
                "single-key output carries a redeem or witness script",
            ));
        }
        Ok(if spk.is_p2wpkh() {
            single(&spk.as_bytes()[2..22], SCRIPT_TYPE_WITNESS_V0, None)
        } else {
            single(&spk.as_bytes()[3..23], SCRIPT_TYPE_BASE, None)
        })
    } else if spk.is_p2sh() {
        match scripts {
            (Some(redeem), None) if redeem.to_p2sh() == spk && redeem.is_p2wpkh() => {
                let program = redeem.as_bytes()[2..22].to_vec();
                Ok(single(&program, SCRIPT_TYPE_WITNESS_V0, Some(redeem)))
            }
            _ => Err(unsupported(
                "only a committed P2SH-wrapped P2WPKH is supported",
            )),
        }
    } else if spk.is_p2wsh() {
        let witness_script = match scripts {
            (None, Some(script)) if script.to_p2wsh() == spk => script,
            _ => {
                return Err(unsupported(
                    "P2WSH needs exactly its committed witness script",
                ))
            }
        };
        let miniscript =
            Miniscript::<PublicKey, Segwitv0>::parse_with_ext(&witness_script, &ExtParams::sane())
                .map_err(|err| unsupported(&err.to_string()))?;
        let Terminal::Multi(thresh) = &miniscript.node else {
            return Err(unsupported("P2WSH witness script is not a top-level multi"));
        };
        Ok(InputContext {
            spent_output,
            script_code: witness_script.clone(),
            script_type: SCRIPT_TYPE_WITNESS_V0,
            keys: Keys::Multi {
                threshold: thresh.k(),
                keys: thresh.data().to_vec(),
            },
            script: Some(witness_script),
        })
    } else {
        Err(unsupported("not P2WPKH, P2SH-P2WPKH, P2PKH or P2WSH multi"))
    }
}

fn verify_with_contexts<C: secp256k1::Verification>(
    psbt: &UnifiedPsbt,
    secp: &secp256k1::Secp256k1<C>,
    contexts: &[InputContext],
) -> Result<usize, ForeignUnifiedError> {
    let records = unified_signatures(psbt)?;
    for record in &records {
        let input = record.input_index;
        require_compatible_sighash(input, psbt.psbt().inputs[input].sighash_type)?;
        if !contexts[input].uses_key(&record.public_key) {
            return Err(ForeignUnifiedError::KeyNotInScript {
                input,
                public_key: record.public_key,
            });
        }
    }
    let spent: Vec<_> = contexts.iter().map(|c| c.spent_output.clone()).collect();
    let cache = UnifiedSighashCache::new(&psbt.psbt().unsigned_tx, &spent)?;
    for record in &records {
        verify_record(&cache, &contexts[record.input_index], record, secp)?;
    }
    Ok(records.len())
}

/// Verify one record. The trailing byte must be exactly `ALL|UNIFIED`: the
/// adapter already refuses anything else, but the digest below is computed
/// for `0x21` only, so this boundary does not rely on it.
fn verify_record<C: secp256k1::Verification>(
    cache: &UnifiedSighashCache<'_>,
    context: &InputContext,
    record: &UnifiedSignature,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<(), ForeignUnifiedError> {
    let invalid = || ForeignUnifiedError::InvalidUnifiedSignature {
        input: record.input_index,
        public_key: record.public_key,
    };
    let der = match record.signature.split_last() {
        Some((&UNIFIED_SIGHASH_ALL, der)) => der,
        _ => return Err(invalid()),
    };
    let digest = cache.signature_hash(
        record.input_index,
        UNIFIED_SIGHASH_ALL,
        context.script_type,
        &context.script_code,
    )?;
    let signature = secp256k1::ecdsa::Signature::from_der(der).map_err(|_| invalid())?;
    secp.verify_ecdsa(
        &secp256k1::Message::from_digest(digest),
        &signature,
        &record.public_key.inner,
    )
    .map_err(|_| invalid())
}

fn require_compatible_sighash(
    input: usize,
    requested: Option<PsbtSighashType>,
) -> Result<(), ForeignUnifiedError> {
    match requested.map(|r| r.to_u32()) {
        None => Ok(()),
        Some(actual) if actual == u32::from(UNIFIED_SIGHASH_ALL) => Ok(()),
        Some(actual) => Err(ForeignUnifiedError::IncompatibleSighash { input, actual }),
    }
}

fn proprietary_key(public_key: &PublicKey) -> ProprietaryKey {
    ProprietaryKey {
        prefix: PROPRIETARY_PREFIX.to_vec(),
        subtype: PROPRIETARY_SUBTYPE,
        key: public_key.to_bytes(),
    }
}

#[cfg(test)]
#[path = "unified_foreign/tests.rs"]
mod tests;
