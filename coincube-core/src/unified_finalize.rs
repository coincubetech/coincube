//! Finalisation of native P2WSH Vault spends that carry Bitcoin Blake2b unified
//! signatures (option (b) of the desktop plan's PR 6: a core finaliser that
//! assembles each input's witness from the miniscript satisfaction *after*
//! cryptographic verification, with no node involved).
//!
//! Why not the node's `finalizepsbt` (option (a)): it needs a local Bitcoin
//! Blake2b node, and the approved default backend for a BTCB2 Cube is Connect
//! Esplora with no local node (`#276` correction 4). rust-miniscript's own
//! finaliser is not an option either: it reads `PSBT_IN_PARTIAL_SIG`, whose
//! typed decoder rejects the `0x21` sighash byte, and its interpreter cannot
//! check a unified digest. So the witness is assembled here from the same
//! miniscript satisfaction logic, fed only with signatures this module has
//! verified itself.
//!
//! What a witness may contain, per key: a **unified** signature (`0x21`,
//! verified by [`verify_p2wsh_all_unified`]) or, failing that, a **legacy**
//! `SIGHASH_ALL` signature from `partial_sigs` (verified here against the
//! BIP-143 digest). A key that has a verified unified signature never
//! contributes its legacy one, an input is first satisfied from unified
//! signatures alone, and legacy signatures are offered only when that is
//! impossible. Offering them is not enough on its own: miniscript picks the
//! cheapest satisfaction, all ECDSA signatures weigh the same, and a `multi`
//! takes keys in script order — so with more signatures than the threshold
//! needs it can build an all-legacy witness for an input that had a unified
//! signature on a later key. [`satisfy_preferring_unified`] catches that case
//! and searches the legacy subsets for a satisfaction that keeps a unified
//! signature; the witness only falls back to legacy-only when **no** offered
//! subset lets the script use one (for instance a unified signature on a
//! recovery key whose timelock this transaction does not enable). That is the
//! "never drop a verified unified witness in favour of a legacy one" rule;
//! when the search cannot be run to completion the input is refused, not
//! degraded. Every other sighash type, `ANYONECANPAY` included, is refused
//! rather than warned about.
//!
//! The result reports, per input, how many unified and legacy signatures ended
//! up in the witness. That is the *only* basis a caller may use for a replay
//! statement (`#276` correction 1): an input whose final witness holds at least
//! one verified unified signature cannot be replayed on Bitcoin, because that
//! signature is invalid there; an input satisfied by legacy signatures alone is
//! replayable, whatever the PSBT claimed. Nothing here asserts chain inclusion,
//! finality, poison evidence or policy authorisation.

use std::{collections::BTreeMap, error, fmt};

use miniscript::{
    bitcoin::{
        absolute, ecdsa,
        hashes::{hash160, Hash},
        secp256k1,
        sighash::{EcdsaSighashType, SighashCache},
        Amount, PublicKey, ScriptBuf, Transaction, TxOut, Witness,
    },
    ExtParams, Miniscript, Satisfier, Segwitv0,
};

use crate::{
    psbt_unified::{unified_signatures, UnifiedPsbt, UnifiedPsbtError},
    spend::{authenticate_previous_output, InputAuthError},
    unified_signing::{verify_p2wsh_all_unified, UnifiedSigningError},
};

/// `SIGHASH_ALL | SIGHASH_UNIFIED`, the only sighash a unified record carries.
const UNIFIED_SIGHASH_ALL: u8 = 0x21;

/// How an input's final witness was built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputWitnessReport {
    /// Verified unified (`0x21`) signatures placed in the witness.
    pub unified_used: usize,
    /// Verified legacy (`SIGHASH_ALL`) signatures placed in the witness.
    pub legacy_used: usize,
}

impl InputWitnessReport {
    /// Whether this input's witness is invalid on Bitcoin: true iff at least one
    /// verified unified signature is part of it. This is a statement about the
    /// bytes in the witness, not about intent or capability.
    pub fn replay_protected(&self) -> bool {
        self.unified_used > 0
    }
}

/// A fully signed transaction and, per input, what its witness is made of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizedSpend {
    pub transaction: Transaction,
    pub inputs: Vec<InputWitnessReport>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifiedFinalizeError {
    /// The PSBT or one of its unified signatures failed verification.
    Signing(UnifiedSigningError),
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
    /// A legacy partial signature with a sighash other than `SIGHASH_ALL`
    /// (`ANYONECANPAY` and friends). Refused, never assembled.
    UnsupportedLegacySighash {
        input: usize,
        public_key: PublicKey,
        sighash: u32,
    },
    /// A legacy partial signature that does not verify against the BIP-143
    /// digest of this transaction.
    InvalidLegacySignature {
        input: usize,
        public_key: PublicKey,
    },
    /// Not enough verified signatures (unified plus, where allowed, legacy) to
    /// satisfy the input's script.
    Unsatisfiable {
        input: usize,
        reason: String,
    },
    /// Internal invariant: the assembled witness contained a signature this
    /// module did not place. Never expected; refused rather than broadcast.
    UnexpectedWitnessElement {
        input: usize,
    },
    /// The input has a verified unified signature, the offered legacy
    /// signatures alone would satisfy it, and there were too many legacy
    /// candidates to search every subset for a satisfaction that keeps a
    /// unified one. Refused rather than finalised from legacy signatures
    /// alone — the caller can drop surplus legacy signatures and retry.
    RefusedToDropUnified {
        input: usize,
        legacy_candidates: usize,
    },
}

impl fmt::Display for UnifiedFinalizeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Signing(err) => write!(f, "unified signature verification failed: {err}"),
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
            Self::UnsupportedLegacySighash {
                input,
                public_key,
                sighash,
            } => write!(
                f,
                "input {input} legacy signature for {public_key} uses refused sighash 0x{sighash:02x}"
            ),
            Self::InvalidLegacySignature { input, public_key } => write!(
                f,
                "input {input} legacy signature for {public_key} does not verify"
            ),
            Self::Unsatisfiable { input, reason } => {
                write!(f, "input {input} cannot be satisfied: {reason}")
            }
            Self::UnexpectedWitnessElement { input } => write!(
                f,
                "input {input} witness contained a signature this finaliser did not place"
            ),
            Self::RefusedToDropUnified {
                input,
                legacy_candidates,
            } => write!(
                f,
                "input {input} has a verified unified signature but {legacy_candidates} legacy \
                 signatures are too many to search for a witness that keeps it; refusing to \
                 finalise from legacy signatures alone"
            ),
        }
    }
}

impl error::Error for UnifiedFinalizeError {}

impl From<UnifiedSigningError> for UnifiedFinalizeError {
    fn from(value: UnifiedSigningError) -> Self {
        Self::Signing(value)
    }
}

impl From<UnifiedPsbtError> for UnifiedFinalizeError {
    fn from(value: UnifiedPsbtError) -> Self {
        Self::Adapter(value)
    }
}

/// One verified signature available to the satisfier, with the exact bytes it
/// must appear as in the witness.
#[derive(Clone)]
struct AvailableSignature {
    /// The DER-encoded signature (no sighash byte).
    der: ecdsa::Signature,
    /// The bytes to place in the witness: DER followed by the sighash byte.
    witness_bytes: Vec<u8>,
    unified: bool,
}

/// Offers exactly one signature per key, and remembers the placeholder bytes
/// miniscript will write for it so they can be swapped for the real encoding.
struct KeyedSatisfier<'a> {
    available: &'a BTreeMap<PublicKey, AvailableSignature>,
}

impl KeyedSatisfier<'_> {
    // Always `SIGHASH_ALL` here: miniscript cannot represent `0x21`, so the
    // placeholder is `DER || 0x01` and the real bytes are substituted after
    // satisfaction. The DER part is what identifies the element.
    fn placeholder(sig: &AvailableSignature) -> ecdsa::Signature {
        ecdsa::Signature {
            signature: sig.der.signature,
            sighash_type: EcdsaSighashType::All,
        }
    }
}

impl Satisfier<PublicKey> for KeyedSatisfier<'_> {
    fn lookup_ecdsa_sig(&self, pk: &PublicKey) -> Option<ecdsa::Signature> {
        self.available.get(pk).map(Self::placeholder)
    }

    // A witness script parsed back from bytes carries `pkh` leaves as raw key
    // hashes (the Vault recovery leaf is one), so the satisfier has to answer
    // by hash as well: the key whose hash matches, and its signature.
    fn lookup_raw_pkh_pk(&self, hash: &hash160::Hash) -> Option<PublicKey> {
        self.available
            .keys()
            .find(|pk| pk.pubkey_hash().as_byte_array() == hash.as_byte_array())
            .copied()
    }

    fn lookup_raw_pkh_ecdsa_sig(
        &self,
        hash: &hash160::Hash,
    ) -> Option<(PublicKey, ecdsa::Signature)> {
        self.available
            .iter()
            .find(|(pk, _)| pk.pubkey_hash().as_byte_array() == hash.as_byte_array())
            .map(|(pk, sig)| (*pk, Self::placeholder(sig)))
    }
}

struct InputContext {
    spent_output: TxOut,
    witness_script: ScriptBuf,
    miniscript: Miniscript<PublicKey, Segwitv0>,
}

fn input_contexts(psbt: &UnifiedPsbt) -> Result<Vec<InputContext>, UnifiedFinalizeError> {
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
        .map_err(|reason| UnifiedFinalizeError::InputAuthentication {
            input: input_index,
            reason,
        })?;
        if !spent_output.script_pubkey.is_p2wsh() {
            return Err(UnifiedFinalizeError::UnsupportedPrevoutScript { input: input_index });
        }
        let witness_script = input
            .witness_script
            .clone()
            .ok_or(UnifiedFinalizeError::MissingWitnessScript { input: input_index })?;
        if witness_script.to_p2wsh() != spent_output.script_pubkey {
            return Err(UnifiedFinalizeError::WitnessScriptCommitmentMismatch {
                input: input_index,
            });
        }
        let miniscript = Miniscript::<PublicKey, Segwitv0>::parse_with_ext(
            &witness_script,
            &ExtParams::sane().raw_pkh(),
        )
        .map_err(|err| UnifiedFinalizeError::UnsupportedWitnessScript {
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

/// Verify a legacy `partial_sigs` entry against the BIP-143 digest. Only
/// `SIGHASH_ALL` is accepted; anything else — including every `ANYONECANPAY`
/// variant — is refused before any witness is built.
fn verify_legacy_signature<C: secp256k1::Verification>(
    secp: &secp256k1::Secp256k1<C>,
    cache: &mut SighashCache<&Transaction>,
    input_index: usize,
    context: &InputContext,
    public_key: &PublicKey,
    signature: &ecdsa::Signature,
) -> Result<(), UnifiedFinalizeError> {
    if signature.sighash_type != EcdsaSighashType::All {
        return Err(UnifiedFinalizeError::UnsupportedLegacySighash {
            input: input_index,
            public_key: *public_key,
            sighash: signature.sighash_type.to_u32(),
        });
    }
    let digest = cache
        .p2wsh_signature_hash(
            input_index,
            &context.witness_script,
            context.spent_output.value,
            EcdsaSighashType::All,
        )
        .map_err(|e| UnifiedFinalizeError::Unsatisfiable {
            input: input_index,
            reason: format!("legacy sighash: {e}"),
        })?;
    let message = secp256k1::Message::from_digest(digest.to_byte_array());
    secp.verify_ecdsa(&message, &signature.signature, &public_key.inner)
        .map_err(|_| UnifiedFinalizeError::InvalidLegacySignature {
            input: input_index,
            public_key: *public_key,
        })
}

/// Assemble the witness for one input from `available`, or explain why not.
fn satisfy_input(
    input_index: usize,
    context: &InputContext,
    available: &BTreeMap<PublicKey, AvailableSignature>,
    sequence: miniscript::bitcoin::Sequence,
    lock_time: absolute::LockTime,
) -> Result<(Witness, InputWitnessReport), UnifiedFinalizeError> {
    let satisfier = (KeyedSatisfier { available }, sequence, lock_time);
    let stack =
        context
            .miniscript
            .satisfy(satisfier)
            .map_err(|e| UnifiedFinalizeError::Unsatisfiable {
                input: input_index,
                reason: e.to_string(),
            })?;

    // Swap every placeholder (`DER || 0x01`) for the bytes that key's signature
    // must actually carry, counting what was used. An element that looks like a
    // signature but is not one we offered is an invariant violation.
    let placeholders: BTreeMap<Vec<u8>, &AvailableSignature> = available
        .values()
        .map(|sig| {
            let mut placeholder = sig.der.signature.serialize_der().to_vec();
            placeholder.push(EcdsaSighashType::All as u8);
            (placeholder, sig)
        })
        .collect();
    let mut report = InputWitnessReport {
        unified_used: 0,
        legacy_used: 0,
    };
    let mut witness = Witness::new();
    for element in stack {
        if let Some(sig) = placeholders.get(&element) {
            if sig.unified {
                report.unified_used += 1;
            } else {
                report.legacy_used += 1;
            }
            witness.push(&sig.witness_bytes);
        } else if looks_like_der_signature(&element) {
            return Err(UnifiedFinalizeError::UnexpectedWitnessElement { input: input_index });
        } else {
            witness.push(&element);
        }
    }
    witness.push(context.witness_script.as_bytes());
    Ok((witness, report))
}

/// Largest legacy candidate set [`satisfy_preferring_unified`] will search
/// exhaustively: 2^12 satisfactions at ~15 µs each in a release build, about
/// 63 ms for the worst case on one input (measured by Elrond at `0eb042c2`;
/// a 2-of-3 searches four subsets). A Vault path has a handful of keys; a
/// PSBT carrying more legacy signatures than this on one input while also
/// holding a unified one is refused rather than degraded.
const MAX_LEGACY_KEYS_FOR_SEARCH: usize = 12;

/// Satisfy with unified signatures plus legacy ones, but never let a legacy
/// signature crowd out a verified unified one.
///
/// miniscript picks the cheapest satisfaction it can and all ECDSA signatures
/// cost the same, so with more signatures on offer than the threshold needs it
/// may take legacy keys in script order and leave a unified key unused — a
/// replayable witness for an input that had a replay-proof signature. When that
/// happens, and the input has unified signatures at all, every subset of the
/// legacy set is offered alongside all the unified signatures and the
/// satisfaction using the most unified signatures wins; ties go to the smaller
/// witness, then to the first subset in enumeration order. The enumeration is
/// over a `BTreeMap` of keys, so the outcome is a deterministic function of
/// the signatures present. The only way a legacy-only witness comes out of
/// here is when **no** offered subset lets the script use a unified signature
/// — the unified key is then genuinely unusable for this transaction (e.g. a
/// recovery key whose timelock is not enabled) and the input is reported as
/// replayable. Over [`MAX_LEGACY_KEYS_FOR_SEARCH`] the input is refused.
fn satisfy_preferring_unified(
    input_index: usize,
    context: &InputContext,
    unified: &BTreeMap<PublicKey, AvailableSignature>,
    legacy: &BTreeMap<PublicKey, AvailableSignature>,
    sequence: miniscript::bitcoin::Sequence,
    lock_time: absolute::LockTime,
) -> Result<(Witness, InputWitnessReport), UnifiedFinalizeError> {
    let mut all: BTreeMap<PublicKey, AvailableSignature> = unified.clone();
    all.extend(legacy.iter().map(|(k, v)| (*k, v.clone())));
    let (witness, report) = satisfy_input(input_index, context, &all, sequence, lock_time)?;
    if unified.is_empty() || report.unified_used > 0 {
        return Ok((witness, report));
    }

    let legacy_keys: Vec<&PublicKey> = legacy.keys().collect();
    if legacy_keys.len() > MAX_LEGACY_KEYS_FOR_SEARCH {
        return Err(UnifiedFinalizeError::RefusedToDropUnified {
            input: input_index,
            legacy_candidates: legacy_keys.len(),
        });
    }
    let mut best: Option<(Witness, InputWitnessReport)> = None;
    for mask in 0u32..(1u32 << legacy_keys.len()) {
        let mut subset: BTreeMap<PublicKey, AvailableSignature> = unified.clone();
        for (bit, key) in legacy_keys.iter().enumerate() {
            if mask & (1 << bit) != 0 {
                subset.insert(**key, legacy[*key].clone());
            }
        }
        if let Ok((w, r)) = satisfy_input(input_index, context, &subset, sequence, lock_time) {
            if r.unified_used == 0 {
                continue;
            }
            let better = match &best {
                None => true,
                Some((bw, br)) => {
                    r.unified_used > br.unified_used
                        || (r.unified_used == br.unified_used && w.size() < bw.size())
                }
            };
            if better {
                best = Some((w, r));
            }
        }
    }
    Ok(best.unwrap_or((witness, report)))
}

/// A witness element shaped like a DER ECDSA signature with a trailing sighash
/// byte — any trailing byte, so a stray `DER || 0x21` is caught as well as a
/// `DER || 0x01`. Used only to refuse elements the finaliser did not place;
/// script pushes (empty, `1`, hash preimages, keys) never look like this.
fn looks_like_der_signature(element: &[u8]) -> bool {
    match element.split_last() {
        Some((_, der)) => {
            der.len() > 8 && der[0] == 0x30 && secp256k1::ecdsa::Signature::from_der(der).is_ok()
        }
        None => false,
    }
}

/// Finalise every input of `psbt` and return the transaction ready to
/// broadcast, together with what each witness is made of.
///
/// Every unified signature in the PSBT is verified first (an invalid one fails
/// the whole call: nothing is assembled from a PSBT that lies). Then, per
/// input: unified signatures are offered alone; if the script cannot be
/// satisfied from those, verified legacy `SIGHASH_ALL` signatures are added for
/// keys that have **no** unified signature, and the input is tried again with
/// the preference pass of [`satisfy_preferring_unified`], so a unified
/// signature that *can* be part of the witness always is. A caller that wants
/// to refuse replayable inputs uses the report; this function does not decide
/// policy.
pub fn finalize_p2wsh_all_unified<C: secp256k1::Verification>(
    psbt: &UnifiedPsbt,
    secp: &secp256k1::Secp256k1<C>,
) -> Result<FinalizedSpend, UnifiedFinalizeError> {
    // Cryptographic verification of every unified record, with the PSBT's own
    // authentication of prevouts and witness scripts.
    verify_p2wsh_all_unified(psbt, secp)?;
    let contexts = input_contexts(psbt)?;
    let unified = unified_signatures(psbt)?;

    let unsigned_tx = &psbt.psbt().unsigned_tx;
    let mut legacy_cache = SighashCache::new(unsigned_tx);
    let mut transaction = unsigned_tx.clone();
    let mut reports = Vec::with_capacity(contexts.len());

    for (input_index, context) in contexts.iter().enumerate() {
        let input = &psbt.psbt().inputs[input_index];
        let txin = &unsigned_tx.input[input_index];

        // Unified first: verified above; the witness bytes are the record itself.
        let mut available: BTreeMap<PublicKey, AvailableSignature> = BTreeMap::new();
        for record in unified.iter().filter(|r| r.input_index == input_index) {
            // A record is `DER || 0x21` (the adapter validated the shape and the
            // verifier above checked the signature). `ecdsa::Signature::from_slice`
            // cannot parse a 0x21 sighash byte, so the placeholder is rebuilt from
            // the DER part with `SIGHASH_ALL`; the witness gets the record itself.
            debug_assert_eq!(record.signature.last(), Some(&UNIFIED_SIGHASH_ALL));
            let invalid = || {
                UnifiedFinalizeError::Signing(UnifiedSigningError::InvalidUnifiedSignature {
                    input: input_index,
                    public_key: record.public_key,
                })
            };
            let (_, der_part) = record.signature.split_last().ok_or(invalid())?;
            let der = ecdsa::Signature {
                signature: secp256k1::ecdsa::Signature::from_der(der_part)
                    .map_err(|_| invalid())?,
                sighash_type: EcdsaSighashType::All,
            };
            available.insert(
                record.public_key,
                AvailableSignature {
                    der,
                    witness_bytes: record.signature.clone(),
                    unified: true,
                },
            );
        }

        let first = satisfy_input(
            input_index,
            context,
            &available,
            txin.sequence,
            unsigned_tx.lock_time,
        );
        let (witness, report) = match first {
            Ok(done) => done,
            Err(UnifiedFinalizeError::Unsatisfiable { .. }) => {
                // Legacy signatures for keys without a unified one, each verified
                // against the BIP-143 digest and restricted to SIGHASH_ALL.
                let mut legacy: BTreeMap<PublicKey, AvailableSignature> = BTreeMap::new();
                for (public_key, signature) in &input.partial_sigs {
                    if available.contains_key(public_key) {
                        continue;
                    }
                    verify_legacy_signature(
                        secp,
                        &mut legacy_cache,
                        input_index,
                        context,
                        public_key,
                        signature,
                    )?;
                    let mut witness_bytes = signature.signature.serialize_der().to_vec();
                    witness_bytes.push(EcdsaSighashType::All as u8);
                    legacy.insert(
                        *public_key,
                        AvailableSignature {
                            der: *signature,
                            witness_bytes,
                            unified: false,
                        },
                    );
                }
                satisfy_preferring_unified(
                    input_index,
                    context,
                    &available,
                    &legacy,
                    txin.sequence,
                    unsigned_tx.lock_time,
                )?
            }
            Err(other) => return Err(other),
        };
        transaction.input[input_index].witness = witness;
        reports.push(report);
    }

    Ok(FinalizedSpend {
        transaction,
        inputs: reports,
    })
}

/// The amount an input spends, for callers that report fees alongside the
/// witness composition. Authenticated the same way the finaliser does.
pub fn spent_amount(psbt: &UnifiedPsbt, input_index: usize) -> Option<Amount> {
    let txin = psbt.psbt().unsigned_tx.input.get(input_index)?;
    let input = psbt.psbt().inputs.get(input_index)?;
    authenticate_previous_output(
        &txin.previous_output,
        input.non_witness_utxo.as_ref(),
        input.witness_utxo.as_ref(),
    )
    .ok()
    .map(|o| o.value)
}

#[cfg(test)]
#[path = "unified_finalize/tests.rs"]
mod tests;
