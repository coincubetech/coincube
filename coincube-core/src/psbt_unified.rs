//! PSBT storage and boundary conversion for Bitcoin Blake2b unified signatures.
//!
//! rust-bitcoin intentionally rejects non-standard ECDSA sighash bytes in
//! `PSBT_IN_PARTIAL_SIG`. Internally, Coincube therefore stores unified ECDSA
//! signatures as input proprietary entries with prefix `coincube`, subtype 0,
//! key data equal to the serialized public key, and value equal to the exact
//! strict-DER signature followed by `SIGHASH_ALL | SIGHASH_UNIFIED` (`0x21`).
//! Standard exports convert those entries back to `PSBT_IN_PARTIAL_SIG`.
//!
//! This module validates representation only. Successful parsing does not prove
//! that a signature authorizes a transaction, belongs to a signer, or prevents
//! replay. Those checks belong at later signing and transaction boundaries.

use std::{
    collections::{BTreeMap, BTreeSet},
    convert::TryFrom,
    error, fmt,
};

use miniscript::bitcoin::{
    self, consensus,
    psbt::{raw::ProprietaryKey, Psbt},
    PublicKey, Transaction,
};

/// Maximum accepted serialized PSBT size at adapter boundaries (16 MiB).
///
/// The limit is checked before parsing and after every conversion. It bounds
/// raw-map slices and allocations independently of rust-bitcoin's own limits.
pub const MAX_PSBT_BYTES: usize = 16 * 1024 * 1024;

const PSBT_MAGIC: &[u8; 5] = b"psbt\xff";
const PSBT_GLOBAL_UNSIGNED_TX: u8 = 0x00;
const PSBT_GLOBAL_VERSION: u8 = 0xfb;
const PSBT_IN_PARTIAL_SIG: u8 = 0x02;
const PSBT_PROPRIETARY: u8 = 0xfc;
const UNIFIED_SIGHASH_ALL: u8 = 0x21;
/// Plain `SIGHASH_ALL`, the only other sighash request a Blake2b Vault PSBT
/// may carry (a legacy signer's request on an input with no unified record).
const LEGACY_SIGHASH_ALL: u8 = 0x01;
const PROPRIETARY_PREFIX: &[u8] = b"coincube";
const PROPRIETARY_SUBTYPE: u8 = 0;
// CompactSize(1) + key 0xfb + CompactSize(4) + four-byte version zero.
const EXPLICIT_GLOBAL_VERSION_SERIALIZED_SIZE: usize = 7;

/// A validated unified ECDSA partial signature stored in a PSBT input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedSignature {
    /// The input map containing the signature.
    pub input_index: usize,
    /// The full compressed or uncompressed ECDSA public key.
    pub public_key: PublicKey,
    /// Exact strict-DER signature bytes followed by `0x21`.
    pub signature: Vec<u8>,
}

/// A typed internal PSBT together with wire-presence information rust-bitcoin
/// does not retain for version zero.
///
/// The PSBT remains mutable for later signing integrations. Every adapter
/// boundary validates the current typed value again before returning bytes,
/// signatures, or applying a merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnifiedPsbt {
    psbt: Psbt,
    explicit_global_version: bool,
}

impl UnifiedPsbt {
    /// Wrap a typed version-zero PSBT whose global version field was absent.
    pub fn from_psbt(psbt: Psbt) -> Result<Self, UnifiedPsbtError> {
        let result = Self {
            psbt,
            explicit_global_version: false,
        };
        validate_internal(&result)?;
        Ok(result)
    }

    /// Read the underlying rust-bitcoin PSBT.
    pub fn psbt(&self) -> &Psbt {
        &self.psbt
    }

    /// Mutate the underlying rust-bitcoin PSBT.
    ///
    /// Mutations are accepted only when a subsequent adapter operation passes
    /// full canonical validation.
    pub fn psbt_mut(&mut self) -> &mut Psbt {
        &mut self.psbt
    }

    /// Whether the source wire map explicitly contained
    /// `PSBT_GLOBAL_VERSION = 0`.
    pub fn has_explicit_global_version(&self) -> bool {
        self.explicit_global_version
    }
}

/// Errors returned by the unified PSBT representation adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifiedPsbtError {
    /// Serialized input exceeded [`MAX_PSBT_BYTES`].
    InputTooLarge { actual: usize, maximum: usize },
    /// The PSBT magic bytes were absent or invalid.
    InvalidMagic,
    /// A CompactSize field was truncated.
    TruncatedCompactSize,
    /// A CompactSize field used a longer-than-minimal encoding.
    NonMinimalCompactSize,
    /// A declared key or value extended beyond the serialized input.
    TruncatedField,
    /// A length could not be represented safely on this platform.
    LengthOverflow,
    /// A raw key appeared more than once in one PSBT map.
    DuplicateRawKey { map: usize, key: Vec<u8> },
    /// A map contained an empty key outside its terminator.
    EmptyKey,
    /// The global map did not contain exactly one valid unsigned transaction.
    MissingUnsignedTransaction,
    /// The unsigned transaction could not be decoded exactly.
    InvalidUnsignedTransaction(String),
    /// The serialized PSBT ended before all transaction-derived maps existed.
    MissingMap { map: usize },
    /// Bytes remained after all expected maps.
    TrailingData,
    /// rust-bitcoin rejected the typed PSBT.
    TypedPsbt(String),
    /// A typed PSBT contained a raw-key alias for a canonical known field.
    NonCanonicalTypedMap,
    /// An unsigned transaction input contained a scriptSig.
    UnsignedTransactionHasScriptSig { input: usize },
    /// An unsigned transaction input contained witness data.
    UnsignedTransactionHasWitness { input: usize },
    /// Only PSBT version 0 is supported by this adapter.
    UnsupportedVersion(u32),
    /// Input or output map counts disagree with the unsigned transaction.
    MapCountMismatch {
        tx_inputs: usize,
        psbt_inputs: usize,
        tx_outputs: usize,
        psbt_outputs: usize,
    },
    /// A reserved entry contained an invalid ECDSA public key.
    InvalidPublicKey { input: usize },
    /// A reserved or standard unified entry did not contain strict DER.
    InvalidDerSignature { input: usize },
    /// The signature namespace appeared in the global map.
    ReservedNamespaceInGlobal,
    /// The signature namespace appeared in an output map.
    ReservedNamespaceInOutput { output: usize },
    /// Unified ECDSA currently accepts only ALL|UNIFIED (`0x21`).
    UnsupportedUnifiedSighash { input: usize, sighash: u8 },
    /// One input/pubkey used both standard and proprietary encodings.
    AmbiguousSignatureEncoding { input: usize, public_key: PublicKey },
    /// The input's `PSBT_IN_SIGHASH_TYPE` request is neither absent,
    /// `SIGHASH_ALL` nor `ALL|UNIFIED`. Refused at the adapter so every
    /// boundary that parses, merges or stores a Blake2b PSBT — not only the
    /// finaliser — rejects an `ANYONECANPAY` (or otherwise unsupported)
    /// request before it is "apparently collected".
    UnsupportedSighashRequest { input: usize, sighash: u32 },
    /// A standard `partial_sigs` entry whose sighash flag is not `SIGHASH_ALL`
    /// (`ANYONECANPAY` and friends). A flag byte in the map, checkable
    /// without a secp context or prevouts, so it lives beside the request
    /// rule and every boundary inherits it; the finaliser's digest selection
    /// checks it again on its own path.
    UnsupportedLegacySighash {
        input: usize,
        public_key: PublicKey,
        sighash: u32,
    },
    /// The PSBTs do not describe the same unsigned transaction.
    UnsignedTransactionMismatch,
    /// A requested input map does not exist.
    InputIndexOutOfBounds { index: usize, inputs: usize },
    /// Two signatures for one input/pubkey differ.
    ConflictingSignature { input: usize, public_key: PublicKey },
}

impl fmt::Display for UnifiedPsbtError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLarge { actual, maximum } => {
                write!(f, "PSBT is {actual} bytes; maximum is {maximum}")
            }
            Self::InvalidMagic => write!(f, "invalid PSBT magic bytes"),
            Self::TruncatedCompactSize => write!(f, "truncated CompactSize"),
            Self::NonMinimalCompactSize => write!(f, "non-minimal CompactSize"),
            Self::TruncatedField => write!(f, "truncated PSBT key or value"),
            Self::LengthOverflow => write!(f, "PSBT length does not fit this platform"),
            Self::DuplicateRawKey { map, .. } => write!(f, "duplicate raw key in map {map}"),
            Self::EmptyKey => write!(f, "empty PSBT key"),
            Self::MissingUnsignedTransaction => {
                write!(f, "PSBT global map has no unsigned transaction")
            }
            Self::InvalidUnsignedTransaction(err) => {
                write!(f, "invalid unsigned transaction: {err}")
            }
            Self::MissingMap { map } => write!(f, "PSBT is missing map {map}"),
            Self::TrailingData => write!(f, "trailing data after final PSBT map"),
            Self::TypedPsbt(err) => write!(f, "invalid typed PSBT: {err}"),
            Self::NonCanonicalTypedMap => {
                write!(f, "typed PSBT contains a noncanonical raw-key alias")
            }
            Self::UnsignedTransactionHasScriptSig { input } => write!(
                f,
                "unsigned transaction input {input} contains a scriptSig"
            ),
            Self::UnsignedTransactionHasWitness { input } => write!(
                f,
                "unsigned transaction input {input} contains witness data"
            ),
            Self::UnsupportedVersion(version) => {
                write!(f, "PSBT version {version} is unsupported")
            }
            Self::MapCountMismatch {
                tx_inputs,
                psbt_inputs,
                tx_outputs,
                psbt_outputs,
            } => write!(
                f,
                "PSBT maps disagree with transaction: {psbt_inputs}/{tx_inputs} inputs, {psbt_outputs}/{tx_outputs} outputs"
            ),
            Self::InvalidPublicKey { input } => {
                write!(f, "invalid partial-signature public key in input {input}")
            }
            Self::InvalidDerSignature { input } => {
                write!(f, "invalid strict-DER unified signature in input {input}")
            }
            Self::ReservedNamespaceInGlobal => {
                write!(f, "unified signature namespace is reserved for input maps, not global")
            }
            Self::ReservedNamespaceInOutput { output } => write!(
                f,
                "unified signature namespace is reserved for input maps, not output {output}"
            ),
            Self::UnsupportedUnifiedSighash { input, sighash } => write!(
                f,
                "unsupported unified sighash 0x{sighash:02x} in input {input}"
            ),
            Self::UnsupportedSighashRequest { input, sighash } => write!(
                f,
                "input {input} asks for sighash 0x{sighash:02x}, which is neither SIGHASH_ALL nor \
                 ALL|UNIFIED"
            ),
            Self::UnsupportedLegacySighash {
                input,
                public_key,
                sighash,
            } => write!(
                f,
                "input {input} legacy signature for {public_key} uses sighash 0x{sighash:02x}; only \
                 SIGHASH_ALL is supported"
            ),
            Self::AmbiguousSignatureEncoding { input, public_key } => write!(
                f,
                "input {input} carries both encodings for {public_key}"
            ),
            Self::UnsignedTransactionMismatch => {
                write!(f, "cannot merge signatures for different transactions")
            }
            Self::InputIndexOutOfBounds { index, inputs } => {
                write!(f, "input index {index} is out of bounds for {inputs} inputs")
            }
            Self::ConflictingSignature { input, public_key } => {
                write!(f, "conflicting signature for {public_key} in input {input}")
            }
        }
    }
}

impl error::Error for UnifiedPsbtError {}

/// Import standard PSBT bytes, moving `0x21` ECDSA partial signatures into the
/// reserved Coincube proprietary namespace.
pub fn import_standard(bytes: &[u8]) -> Result<UnifiedPsbt, UnifiedPsbtError> {
    let mut raw = RawPsbt::parse(bytes)?;
    let explicit_global_version = raw.has_explicit_global_version();
    for (input_index, map) in raw.inputs.iter_mut().enumerate() {
        convert_standard_input_to_internal(map, input_index)?;
    }
    let internal = raw.serialize()?;
    let psbt = Psbt::deserialize(&internal).map_err(typed_error)?;
    let result = UnifiedPsbt {
        psbt,
        explicit_global_version,
    };
    validate_internal(&result)?;
    Ok(result)
}

/// Deserialize Coincube's internal typed representation with strict raw-map
/// duplicate, length, map-count, and trailing-data checks.
pub fn deserialize_internal(bytes: &[u8]) -> Result<UnifiedPsbt, UnifiedPsbtError> {
    let raw = RawPsbt::parse(bytes)?;
    let explicit_global_version = raw.has_explicit_global_version();
    let psbt = Psbt::deserialize(bytes).map_err(typed_error)?;
    let result = UnifiedPsbt {
        psbt,
        explicit_global_version,
    };
    validate_internal(&result)?;
    Ok(result)
}

/// Serialize Coincube's internal representation after validating it again.
pub fn serialize_internal(psbt: &UnifiedPsbt) -> Result<Vec<u8>, UnifiedPsbtError> {
    validate_internal(psbt)?;
    serialize_with_version_presence(psbt)
}

/// Validate a mutable/public rust-bitcoin PSBT as Coincube's internal form.
pub fn validate_internal(psbt: &UnifiedPsbt) -> Result<(), UnifiedPsbtError> {
    let typed_size = validate_typed_psbt(&psbt.psbt)?;
    let version_size = if psbt.explicit_global_version {
        EXPLICIT_GLOBAL_VERSION_SERIALIZED_SIZE
    } else {
        0
    };
    let wrapper_size = typed_size
        .checked_add(version_size)
        .ok_or(UnifiedPsbtError::LengthOverflow)?;
    ensure_size(wrapper_size)
}

fn validate_typed_psbt(psbt: &Psbt) -> Result<usize, UnifiedPsbtError> {
    if psbt.version != 0 {
        return Err(UnifiedPsbtError::UnsupportedVersion(psbt.version));
    }
    if psbt.inputs.len() != psbt.unsigned_tx.input.len()
        || psbt.outputs.len() != psbt.unsigned_tx.output.len()
    {
        return Err(UnifiedPsbtError::MapCountMismatch {
            tx_inputs: psbt.unsigned_tx.input.len(),
            psbt_inputs: psbt.inputs.len(),
            tx_outputs: psbt.unsigned_tx.output.len(),
            psbt_outputs: psbt.outputs.len(),
        });
    }

    for (input, txin) in psbt.unsigned_tx.input.iter().enumerate() {
        if !txin.script_sig.is_empty() {
            return Err(UnifiedPsbtError::UnsignedTransactionHasScriptSig { input });
        }
        if !txin.witness.is_empty() {
            return Err(UnifiedPsbtError::UnsignedTransactionHasWitness { input });
        }
    }

    let serialized = psbt.serialize();
    ensure_size(serialized.len())?;
    RawPsbt::parse(&serialized)?;
    let canonical = Psbt::deserialize(&serialized).map_err(typed_error)?;
    if canonical != *psbt {
        return Err(UnifiedPsbtError::NonCanonicalTypedMap);
    }

    if psbt.proprietary.keys().any(is_reserved_key) {
        return Err(UnifiedPsbtError::ReservedNamespaceInGlobal);
    }
    for (output, map) in psbt.outputs.iter().enumerate() {
        if map.proprietary.keys().any(is_reserved_key) {
            return Err(UnifiedPsbtError::ReservedNamespaceInOutput { output });
        }
    }

    for (input_index, input) in psbt.inputs.iter().enumerate() {
        // A sighash *request* the chain does not serve is invalid here, not
        // just at finalisation: absent, `SIGHASH_ALL` or `ALL|UNIFIED`. (An
        // input that also carries a unified record is held to the stricter
        // rule — absent or `ALL|UNIFIED` — by the verifier.)
        if let Some(requested) = input.sighash_type {
            let raw = requested.to_u32();
            if raw != u32::from(LEGACY_SIGHASH_ALL) && raw != u32::from(UNIFIED_SIGHASH_ALL) {
                return Err(UnifiedPsbtError::UnsupportedSighashRequest {
                    input: input_index,
                    sighash: raw,
                });
            }
        }
        // And every standard signature's own flag: `SIGHASH_ALL` only.
        for (public_key, signature) in &input.partial_sigs {
            let raw = signature.sighash_type.to_u32();
            if raw != u32::from(LEGACY_SIGHASH_ALL) {
                return Err(UnifiedPsbtError::UnsupportedLegacySighash {
                    input: input_index,
                    public_key: *public_key,
                    sighash: raw,
                });
            }
        }
        for (key, value) in &input.proprietary {
            if !is_reserved_key(key) {
                continue;
            }
            let public_key = parse_public_key(&key.key, input_index)?;
            validate_signature(value, input_index)?;
            if input.partial_sigs.contains_key(&public_key) {
                return Err(UnifiedPsbtError::AmbiguousSignatureEncoding {
                    input: input_index,
                    public_key,
                });
            }
        }
    }
    Ok(serialized.len())
}

/// Export internal PSBT state as standard BIP174 bytes with unified signatures
/// restored to `PSBT_IN_PARTIAL_SIG` entries.
pub fn export_standard(psbt: &UnifiedPsbt) -> Result<Vec<u8>, UnifiedPsbtError> {
    validate_internal(psbt)?;
    let internal = serialize_with_version_presence(psbt)?;
    let mut raw = RawPsbt::parse(&internal)?;
    for (input_index, map) in raw.inputs.iter_mut().enumerate() {
        convert_internal_input_to_standard(map, input_index)?;
    }
    raw.serialize()
}

/// Return all validated unified signatures without changing the PSBT.
pub fn unified_signatures(psbt: &UnifiedPsbt) -> Result<Vec<UnifiedSignature>, UnifiedPsbtError> {
    validate_internal(psbt)?;
    let mut signatures = Vec::new();
    for (input_index, input) in psbt.psbt.inputs.iter().enumerate() {
        for (key, value) in &input.proprietary {
            if is_reserved_key(key) {
                signatures.push(UnifiedSignature {
                    input_index,
                    public_key: parse_public_key(&key.key, input_index)?,
                    signature: value.clone(),
                });
            }
        }
    }
    Ok(signatures)
}

/// Merge only ECDSA partial signatures from every input in `delta`.
///
/// All validation and conflict checks happen on a clone. `destination` is
/// assigned only after the complete merge succeeds, so failures are atomic.
pub fn merge_signatures(
    destination: &mut UnifiedPsbt,
    delta: &UnifiedPsbt,
) -> Result<(), UnifiedPsbtError> {
    validate_merge_pair(destination, delta)?;
    let mut merged = destination.clone();
    for input_index in 0..merged.psbt.inputs.len() {
        merge_one_input(&mut merged, delta, input_index)?;
    }
    validate_internal(&merged)?;
    *destination = merged;
    Ok(())
}

/// Merge only one input's ECDSA partial signatures from `delta`.
pub fn merge_input_signatures(
    destination: &mut UnifiedPsbt,
    delta: &UnifiedPsbt,
    input_index: usize,
) -> Result<(), UnifiedPsbtError> {
    validate_merge_pair(destination, delta)?;
    if input_index >= destination.psbt.inputs.len() {
        return Err(UnifiedPsbtError::InputIndexOutOfBounds {
            index: input_index,
            inputs: destination.psbt.inputs.len(),
        });
    }
    let mut merged = destination.clone();
    merge_one_input(&mut merged, delta, input_index)?;
    validate_internal(&merged)?;
    *destination = merged;
    Ok(())
}

fn validate_merge_pair(
    destination: &UnifiedPsbt,
    delta: &UnifiedPsbt,
) -> Result<(), UnifiedPsbtError> {
    validate_internal(destination)?;
    validate_internal(delta)?;
    if destination.psbt.unsigned_tx != delta.psbt.unsigned_tx {
        return Err(UnifiedPsbtError::UnsignedTransactionMismatch);
    }
    Ok(())
}

fn merge_one_input(
    destination: &mut UnifiedPsbt,
    delta: &UnifiedPsbt,
    input_index: usize,
) -> Result<(), UnifiedPsbtError> {
    let delta_input = &delta.psbt.inputs[input_index];
    let destination_input = &mut destination.psbt.inputs[input_index];

    let destination_unified = reserved_signatures(&destination_input.proprietary, input_index)?;
    let delta_unified = reserved_signatures(&delta_input.proprietary, input_index)?;

    for (public_key, signature) in &delta_input.partial_sigs {
        if destination_unified.contains_key(public_key) {
            return Err(UnifiedPsbtError::AmbiguousSignatureEncoding {
                input: input_index,
                public_key: *public_key,
            });
        }
        match destination_input.partial_sigs.get(public_key) {
            Some(existing) if existing != signature => {
                return Err(UnifiedPsbtError::ConflictingSignature {
                    input: input_index,
                    public_key: *public_key,
                });
            }
            Some(_) => {}
            None => {
                destination_input
                    .partial_sigs
                    .insert(*public_key, *signature);
            }
        }
    }

    for (public_key, (key, signature)) in delta_unified {
        if destination_input.partial_sigs.contains_key(&public_key) {
            return Err(UnifiedPsbtError::AmbiguousSignatureEncoding {
                input: input_index,
                public_key,
            });
        }
        match destination_unified.get(&public_key) {
            Some((_, existing)) if *existing != signature => {
                return Err(UnifiedPsbtError::ConflictingSignature {
                    input: input_index,
                    public_key,
                });
            }
            Some(_) => {}
            None => {
                destination_input.proprietary.insert(key, signature);
            }
        }
    }
    Ok(())
}

fn reserved_signatures(
    entries: &BTreeMap<ProprietaryKey, Vec<u8>>,
    input_index: usize,
) -> Result<BTreeMap<PublicKey, (ProprietaryKey, Vec<u8>)>, UnifiedPsbtError> {
    let mut result = BTreeMap::new();
    for (key, value) in entries {
        if is_reserved_key(key) {
            let public_key = parse_public_key(&key.key, input_index)?;
            validate_signature(value, input_index)?;
            result.insert(public_key, (key.clone(), value.clone()));
        }
    }
    Ok(result)
}

fn convert_standard_input_to_internal(
    map: &mut RawMap,
    input_index: usize,
) -> Result<(), UnifiedPsbtError> {
    let mut standard_keys = BTreeSet::new();
    let mut reserved_keys = BTreeSet::new();

    for pair in &map.pairs {
        if pair.key.first() == Some(&PSBT_IN_PARTIAL_SIG) {
            let public_key = parse_public_key(&pair.key[1..], input_index)?;
            standard_keys.insert(public_key);
            if let Some(sighash) = pair.value.last().copied() {
                if sighash & 0x20 != 0 {
                    if sighash != UNIFIED_SIGHASH_ALL {
                        return Err(UnifiedPsbtError::UnsupportedUnifiedSighash {
                            input: input_index,
                            sighash,
                        });
                    }
                    validate_signature(&pair.value, input_index)?;
                }
            }
        } else if let Some((public_key, _)) = raw_reserved_signature(pair, input_index)? {
            reserved_keys.insert(public_key);
        }
    }

    if let Some(public_key) = standard_keys.intersection(&reserved_keys).next() {
        return Err(UnifiedPsbtError::AmbiguousSignatureEncoding {
            input: input_index,
            public_key: *public_key,
        });
    }

    for pair in &mut map.pairs {
        if pair.key.first() == Some(&PSBT_IN_PARTIAL_SIG)
            && pair.value.last() == Some(&UNIFIED_SIGHASH_ALL)
        {
            let public_key = parse_public_key(&pair.key[1..], input_index)?;
            pair.key = proprietary_raw_key(&public_key);
        }
    }
    Ok(())
}

fn convert_internal_input_to_standard(
    map: &mut RawMap,
    input_index: usize,
) -> Result<(), UnifiedPsbtError> {
    let mut standard_keys = BTreeSet::new();
    for pair in &map.pairs {
        if pair.key.first() == Some(&PSBT_IN_PARTIAL_SIG) {
            standard_keys.insert(parse_public_key(&pair.key[1..], input_index)?);
        }
    }

    for pair in &mut map.pairs {
        if let Some((public_key, signature)) = raw_reserved_signature(pair, input_index)? {
            if standard_keys.contains(&public_key) {
                return Err(UnifiedPsbtError::AmbiguousSignatureEncoding {
                    input: input_index,
                    public_key,
                });
            }
            pair.key = standard_raw_key(&public_key);
            pair.value = signature;
            standard_keys.insert(public_key);
        }
    }
    Ok(())
}

fn raw_reserved_signature(
    pair: &RawPair,
    input_index: usize,
) -> Result<Option<(PublicKey, Vec<u8>)>, UnifiedPsbtError> {
    if pair.key.first() != Some(&PSBT_PROPRIETARY) {
        return Ok(None);
    }
    let key: ProprietaryKey = consensus::deserialize(&pair.key[1..]).map_err(typed_error)?;
    if !is_reserved_key(&key) {
        return Ok(None);
    }
    let public_key = parse_public_key(&key.key, input_index)?;
    validate_signature(&pair.value, input_index)?;
    Ok(Some((public_key, pair.value.clone())))
}

fn is_reserved_key(key: &ProprietaryKey) -> bool {
    key.prefix == PROPRIETARY_PREFIX && key.subtype == PROPRIETARY_SUBTYPE
}

fn proprietary_key(public_key: &PublicKey) -> ProprietaryKey {
    ProprietaryKey {
        prefix: PROPRIETARY_PREFIX.to_vec(),
        subtype: PROPRIETARY_SUBTYPE,
        key: public_key.to_bytes(),
    }
}

fn proprietary_raw_key(public_key: &PublicKey) -> Vec<u8> {
    let key = proprietary_key(public_key).to_key();
    let mut raw = Vec::with_capacity(key.key.len() + 1);
    raw.push(key.type_value);
    raw.extend_from_slice(&key.key);
    raw
}

fn standard_raw_key(public_key: &PublicKey) -> Vec<u8> {
    let mut raw = Vec::with_capacity(public_key.to_bytes().len() + 1);
    raw.push(PSBT_IN_PARTIAL_SIG);
    raw.extend_from_slice(&public_key.to_bytes());
    raw
}

fn parse_public_key(bytes: &[u8], input_index: usize) -> Result<PublicKey, UnifiedPsbtError> {
    PublicKey::from_slice(bytes)
        .map_err(|_| UnifiedPsbtError::InvalidPublicKey { input: input_index })
}

fn validate_signature(value: &[u8], input_index: usize) -> Result<(), UnifiedPsbtError> {
    let (sighash, der) = value
        .split_last()
        .ok_or(UnifiedPsbtError::InvalidDerSignature { input: input_index })?;
    if *sighash != UNIFIED_SIGHASH_ALL {
        return Err(UnifiedPsbtError::UnsupportedUnifiedSighash {
            input: input_index,
            sighash: *sighash,
        });
    }
    let signature = bitcoin::secp256k1::ecdsa::Signature::from_der(der)
        .map_err(|_| UnifiedPsbtError::InvalidDerSignature { input: input_index })?;
    if signature.serialize_der().as_ref() != der {
        return Err(UnifiedPsbtError::InvalidDerSignature { input: input_index });
    }
    Ok(())
}

fn typed_error(err: impl fmt::Display) -> UnifiedPsbtError {
    UnifiedPsbtError::TypedPsbt(err.to_string())
}

fn ensure_size(size: usize) -> Result<(), UnifiedPsbtError> {
    if size > MAX_PSBT_BYTES {
        Err(UnifiedPsbtError::InputTooLarge {
            actual: size,
            maximum: MAX_PSBT_BYTES,
        })
    } else {
        Ok(())
    }
}

fn serialize_with_version_presence(psbt: &UnifiedPsbt) -> Result<Vec<u8>, UnifiedPsbtError> {
    let mut raw = RawPsbt::parse(&psbt.psbt.serialize())?;
    if psbt.explicit_global_version {
        raw.global.pairs.push(RawPair {
            key: vec![PSBT_GLOBAL_VERSION],
            value: 0u32.to_le_bytes().to_vec(),
        });
    }
    raw.serialize()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawPair {
    /// Complete key bytes: type byte followed by key data.
    key: Vec<u8>,
    value: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawMap {
    pairs: Vec<RawPair>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RawPsbt {
    global: RawMap,
    inputs: Vec<RawMap>,
    outputs: Vec<RawMap>,
}

impl RawPsbt {
    fn parse(bytes: &[u8]) -> Result<Self, UnifiedPsbtError> {
        ensure_size(bytes.len())?;
        if bytes.len() < PSBT_MAGIC.len() || &bytes[..PSBT_MAGIC.len()] != PSBT_MAGIC {
            return Err(UnifiedPsbtError::InvalidMagic);
        }
        let mut cursor = Cursor {
            bytes,
            position: PSBT_MAGIC.len(),
        };
        let global = cursor.read_map(0)?;
        require_v0(&global)?;
        let unsigned = global
            .pairs
            .iter()
            .find(|pair| pair.key.as_slice() == [PSBT_GLOBAL_UNSIGNED_TX])
            .ok_or(UnifiedPsbtError::MissingUnsignedTransaction)?;
        let transaction: Transaction = consensus::deserialize(&unsigned.value)
            .map_err(|err| UnifiedPsbtError::InvalidUnsignedTransaction(err.to_string()))?;

        let mut inputs = Vec::new();
        for input_index in 0..transaction.input.len() {
            let map_index = input_index
                .checked_add(1)
                .ok_or(UnifiedPsbtError::LengthOverflow)?;
            inputs.push(cursor.read_map(map_index)?);
        }
        let mut outputs = Vec::new();
        for output_index in 0..transaction.output.len() {
            let map_index = transaction
                .input
                .len()
                .checked_add(output_index)
                .and_then(|index| index.checked_add(1))
                .ok_or(UnifiedPsbtError::LengthOverflow)?;
            outputs.push(cursor.read_map(map_index)?);
        }
        if cursor.position != bytes.len() {
            return Err(UnifiedPsbtError::TrailingData);
        }
        Ok(Self {
            global,
            inputs,
            outputs,
        })
    }

    fn serialize(&self) -> Result<Vec<u8>, UnifiedPsbtError> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(PSBT_MAGIC);
        encode_map(&mut bytes, &self.global)?;
        for map in &self.inputs {
            encode_map(&mut bytes, map)?;
        }
        for map in &self.outputs {
            encode_map(&mut bytes, map)?;
        }
        ensure_size(bytes.len())?;
        Ok(bytes)
    }

    fn has_explicit_global_version(&self) -> bool {
        self.global
            .pairs
            .iter()
            .any(|pair| pair.key.as_slice() == [PSBT_GLOBAL_VERSION])
    }
}

fn require_v0(global: &RawMap) -> Result<(), UnifiedPsbtError> {
    let version = global
        .pairs
        .iter()
        .find(|pair| pair.key.as_slice() == [PSBT_GLOBAL_VERSION]);
    let version = match version {
        None => 0,
        Some(pair) if pair.value.len() == 4 => {
            u32::from_le_bytes([pair.value[0], pair.value[1], pair.value[2], pair.value[3]])
        }
        Some(_) => {
            return Err(UnifiedPsbtError::TypedPsbt(
                "invalid PSBT version field".to_string(),
            ));
        }
    };
    if version != 0 {
        return Err(UnifiedPsbtError::UnsupportedVersion(version));
    }
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl Cursor<'_> {
    fn read_map(&mut self, map_index: usize) -> Result<RawMap, UnifiedPsbtError> {
        if self.position >= self.bytes.len() {
            return Err(UnifiedPsbtError::MissingMap { map: map_index });
        }
        let mut pairs = Vec::new();
        let mut keys = BTreeSet::new();
        loop {
            let key_len = self.read_compact_size()?;
            if key_len == 0 {
                break;
            }
            let key = self.read_exact(key_len)?.to_vec();
            if key.is_empty() {
                return Err(UnifiedPsbtError::EmptyKey);
            }
            if !keys.insert(key.clone()) {
                return Err(UnifiedPsbtError::DuplicateRawKey {
                    map: map_index,
                    key,
                });
            }
            let value_len = self.read_compact_size()?;
            let value = self.read_exact(value_len)?.to_vec();
            pairs.push(RawPair { key, value });
        }
        Ok(RawMap { pairs })
    }

    fn read_compact_size(&mut self) -> Result<usize, UnifiedPsbtError> {
        let tag = *self
            .read_exact(1)?
            .first()
            .ok_or(UnifiedPsbtError::TruncatedCompactSize)?;
        let value = match tag {
            0x00..=0xfc => u64::from(tag),
            0xfd => {
                let raw = self.read_compact_tail(2)?;
                let value = u64::from(u16::from_le_bytes([raw[0], raw[1]]));
                if value < 0xfd {
                    return Err(UnifiedPsbtError::NonMinimalCompactSize);
                }
                value
            }
            0xfe => {
                let raw = self.read_compact_tail(4)?;
                let value = u64::from(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]));
                if value <= u64::from(u16::MAX) {
                    return Err(UnifiedPsbtError::NonMinimalCompactSize);
                }
                value
            }
            0xff => {
                let raw = self.read_compact_tail(8)?;
                let value = u64::from_le_bytes([
                    raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
                ]);
                if value <= u64::from(u32::MAX) {
                    return Err(UnifiedPsbtError::NonMinimalCompactSize);
                }
                value
            }
        };
        usize::try_from(value).map_err(|_| UnifiedPsbtError::LengthOverflow)
    }

    fn read_compact_tail(&mut self, length: usize) -> Result<&[u8], UnifiedPsbtError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(UnifiedPsbtError::LengthOverflow)?;
        if end > self.bytes.len() {
            return Err(UnifiedPsbtError::TruncatedCompactSize);
        }
        let result = &self.bytes[self.position..end];
        self.position = end;
        Ok(result)
    }

    fn read_exact(&mut self, length: usize) -> Result<&[u8], UnifiedPsbtError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(UnifiedPsbtError::LengthOverflow)?;
        if end > self.bytes.len() {
            return Err(if length == 1 {
                UnifiedPsbtError::TruncatedCompactSize
            } else {
                UnifiedPsbtError::TruncatedField
            });
        }
        let result = &self.bytes[self.position..end];
        self.position = end;
        Ok(result)
    }
}

fn encode_map(bytes: &mut Vec<u8>, map: &RawMap) -> Result<(), UnifiedPsbtError> {
    for pair in &map.pairs {
        if pair.key.is_empty() {
            return Err(UnifiedPsbtError::EmptyKey);
        }
        encode_compact_size(bytes, pair.key.len())?;
        bytes.extend_from_slice(&pair.key);
        encode_compact_size(bytes, pair.value.len())?;
        bytes.extend_from_slice(&pair.value);
        ensure_size(bytes.len())?;
    }
    bytes.push(0);
    ensure_size(bytes.len())
}

fn encode_compact_size(bytes: &mut Vec<u8>, value: usize) -> Result<(), UnifiedPsbtError> {
    let value = u64::try_from(value).map_err(|_| UnifiedPsbtError::LengthOverflow)?;
    match value {
        0..=0xfc => bytes.push(u8::try_from(value).map_err(|_| UnifiedPsbtError::LengthOverflow)?),
        0xfd..=0xffff => {
            bytes.push(0xfd);
            bytes.extend_from_slice(
                &u16::try_from(value)
                    .map_err(|_| UnifiedPsbtError::LengthOverflow)?
                    .to_le_bytes(),
            );
        }
        0x1_0000..=0xffff_ffff => {
            bytes.push(0xfe);
            bytes.extend_from_slice(
                &u32::try_from(value)
                    .map_err(|_| UnifiedPsbtError::LengthOverflow)?
                    .to_le_bytes(),
            );
        }
        _ => {
            bytes.push(0xff);
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
