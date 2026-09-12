//! Unified signature hashing for the Bitcoin Blake2b network.
//!
//! This module implements script types 0 (bare/P2SH) and 1 (SegWit v0) from
//! Bitcoin Knots' draft unified-sighash specification. It only computes the
//! message digest; signing policy and PSBT integration live elsewhere.

use std::{convert::TryFrom, error, fmt};

use miniscript::bitcoin::{
    self,
    hashes::{sha256, Hash},
    Script, Transaction, TxOut,
};

/// Opt-in bit selecting the unified signature-hash algorithm.
pub const SIGHASH_UNIFIED: u8 = 0x20;
/// Sign no outputs.
pub const SIGHASH_NONE: u8 = 0x02;
/// Sign the output at the input's index.
pub const SIGHASH_SINGLE: u8 = 0x03;
/// Sign only the current input.
pub const SIGHASH_ANYONECANPAY: u8 = 0x80;

const SIGHASH_OUTPUT_MASK: u8 = 0x1f;
/// Bare or P2SH script type.
pub const SCRIPT_TYPE_BASE: u8 = 0;
/// SegWit-v0 script type.
pub const SCRIPT_TYPE_WITNESS_V0: u8 = 1;
const UNIFIED_SIGHASH_TAG: &[u8] = b"UnifiedSighash";

/// Invalid input to [`unified_sighash`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifiedSighashError {
    /// The hash-type byte did not opt in to unified sighashing.
    MissingUnifiedFlag(u8),
    /// This implementation intentionally supports only script types 0 and 1.
    UnsupportedScriptType(u8),
    /// Unified sighashing needs exactly one spent output for every input.
    PrevoutsLengthMismatch { inputs: usize, prevouts: usize },
    /// The requested input is not present in the transaction.
    InputIndexOutOfBounds { index: usize, inputs: usize },
    /// The input index cannot be represented by the four-byte wire field.
    InputIndexTooLarge(usize),
    /// `SIGHASH_SINGLE` requires an output at the same index as the input.
    MissingSingleOutput { index: usize, outputs: usize },
}

impl fmt::Display for UnifiedSighashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingUnifiedFlag(hash_type) => {
                write!(
                    f,
                    "hash type 0x{hash_type:02x} does not set SIGHASH_UNIFIED"
                )
            }
            Self::UnsupportedScriptType(script_type) => {
                write!(
                    f,
                    "unified sighash script type {script_type} is unsupported"
                )
            }
            Self::PrevoutsLengthMismatch { inputs, prevouts } => write!(
                f,
                "unified sighash needs {inputs} spent outputs, received {prevouts}"
            ),
            Self::InputIndexOutOfBounds { index, inputs } => {
                write!(
                    f,
                    "input index {index} is out of bounds for {inputs} inputs"
                )
            }
            Self::InputIndexTooLarge(index) => {
                write!(
                    f,
                    "input index {index} does not fit the unified sighash wire field"
                )
            }
            Self::MissingSingleOutput { index, outputs } => write!(
                f,
                "SIGHASH_SINGLE input {index} has no matching output ({outputs} outputs)"
            ),
        }
    }
}

impl error::Error for UnifiedSighashError {}

/// Transaction-wide commitments reused by every unified signature hash.
///
/// Construct this once per transaction when hashing more than one input. This
/// keeps aggregate hashing linear in the number of transaction inputs.
#[derive(Debug)]
pub struct UnifiedSighashCache<'a> {
    transaction: &'a Transaction,
    spent_outputs: &'a [TxOut],
    prevouts_hash: [u8; 32],
    amounts_hash: [u8; 32],
    scripts_hash: [u8; 32],
    sequences_hash: [u8; 32],
    outputs_hash: [u8; 32],
}

impl<'a> UnifiedSighashCache<'a> {
    /// Precompute the transaction-wide commitments.
    pub fn new(
        transaction: &'a Transaction,
        spent_outputs: &'a [TxOut],
    ) -> Result<Self, UnifiedSighashError> {
        if spent_outputs.len() != transaction.input.len() {
            return Err(UnifiedSighashError::PrevoutsLengthMismatch {
                inputs: transaction.input.len(),
                prevouts: spent_outputs.len(),
            });
        }

        Ok(Self {
            transaction,
            spent_outputs,
            prevouts_hash: aggregate_prevouts(transaction),
            amounts_hash: aggregate_amounts(spent_outputs),
            scripts_hash: aggregate_scripts(spent_outputs),
            sequences_hash: aggregate_sequences(transaction),
            outputs_hash: aggregate_outputs(&transaction.output),
        })
    }

    /// Compute one input's digest using the cached transaction commitments.
    pub fn signature_hash(
        &self,
        input_index: usize,
        hash_type: u8,
        script_type: u8,
        script_code: &Script,
    ) -> Result<[u8; 32], UnifiedSighashError> {
        let message =
            unified_sighash_message(self, input_index, hash_type, script_type, script_code)?;
        Ok(tagged_hash(&message))
    }
}

/// Compute the unified signature hash for a bare/P2SH or SegWit-v0 input.
///
/// `spent_outputs` must contain the output spent by every transaction input in
/// input order. `hash_type` is the exact byte carried by the signature; bits
/// without defined legacy meaning are still committed to and are not erased.
pub fn unified_sighash(
    transaction: &Transaction,
    input_index: usize,
    hash_type: u8,
    script_type: u8,
    spent_outputs: &[TxOut],
    script_code: &Script,
) -> Result<[u8; 32], UnifiedSighashError> {
    UnifiedSighashCache::new(transaction, spent_outputs)?.signature_hash(
        input_index,
        hash_type,
        script_type,
        script_code,
    )
}

fn unified_sighash_message(
    cache: &UnifiedSighashCache<'_>,
    input_index: usize,
    hash_type: u8,
    script_type: u8,
    script_code: &Script,
) -> Result<Vec<u8>, UnifiedSighashError> {
    let transaction = cache.transaction;
    if hash_type & SIGHASH_UNIFIED == 0 {
        return Err(UnifiedSighashError::MissingUnifiedFlag(hash_type));
    }
    if script_type != SCRIPT_TYPE_BASE && script_type != SCRIPT_TYPE_WITNESS_V0 {
        return Err(UnifiedSighashError::UnsupportedScriptType(script_type));
    }
    if input_index >= transaction.input.len() {
        return Err(UnifiedSighashError::InputIndexOutOfBounds {
            index: input_index,
            inputs: transaction.input.len(),
        });
    }
    let input_index_u32 = u32::try_from(input_index)
        .map_err(|_| UnifiedSighashError::InputIndexTooLarge(input_index))?;
    let output_type = hash_type & SIGHASH_OUTPUT_MASK;
    if output_type == SIGHASH_SINGLE && input_index >= transaction.output.len() {
        return Err(UnifiedSighashError::MissingSingleOutput {
            index: input_index,
            outputs: transaction.output.len(),
        });
    }

    let anyone_can_pay = hash_type & SIGHASH_ANYONECANPAY != 0;
    let mut message = Vec::new();

    message.push(0); // epoch
    message.push(hash_type);
    message.extend_from_slice(&transaction.version.0.to_le_bytes());
    message.extend_from_slice(&transaction.lock_time.to_consensus_u32().to_le_bytes());
    message.push(0); // fifth, zero-extended locktime byte

    if !anyone_can_pay {
        message.extend_from_slice(&cache.prevouts_hash);
        message.extend_from_slice(&cache.amounts_hash);
        message.extend_from_slice(&cache.scripts_hash);
        message.extend_from_slice(&cache.sequences_hash);
    }
    if output_type != SIGHASH_NONE && output_type != SIGHASH_SINGLE {
        message.extend_from_slice(&cache.outputs_hash);
    }

    message.push(script_type);
    if anyone_can_pay {
        append_consensus(
            &mut message,
            &transaction.input[input_index].previous_output,
        );
        append_consensus(&mut message, &cache.spent_outputs[input_index]);
        append_consensus(&mut message, &transaction.input[input_index].sequence);
    } else {
        message.extend_from_slice(&input_index_u32.to_le_bytes());
    }
    append_consensus(&mut message, script_code);

    if output_type == SIGHASH_SINGLE {
        message.extend_from_slice(&sha256_bytes(&bitcoin::consensus::serialize(
            &transaction.output[input_index],
        )));
    }

    Ok(message)
}

fn aggregate_prevouts(transaction: &Transaction) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(transaction.input.len() * 36);
    for input in &transaction.input {
        append_consensus(&mut bytes, &input.previous_output);
    }
    sha256_bytes(&bytes)
}

fn aggregate_amounts(spent_outputs: &[TxOut]) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(spent_outputs.len() * 8);
    for output in spent_outputs {
        append_consensus(&mut bytes, &output.value);
    }
    sha256_bytes(&bytes)
}

fn aggregate_scripts(spent_outputs: &[TxOut]) -> [u8; 32] {
    let mut bytes = Vec::new();
    for output in spent_outputs {
        append_consensus(&mut bytes, output.script_pubkey.as_script());
    }
    sha256_bytes(&bytes)
}

fn aggregate_sequences(transaction: &Transaction) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(transaction.input.len() * 4);
    for input in &transaction.input {
        append_consensus(&mut bytes, &input.sequence);
    }
    sha256_bytes(&bytes)
}

fn aggregate_outputs(outputs: &[TxOut]) -> [u8; 32] {
    let mut bytes = Vec::new();
    for output in outputs {
        append_consensus(&mut bytes, output);
    }
    sha256_bytes(&bytes)
}

fn append_consensus<T: bitcoin::consensus::Encodable + ?Sized>(bytes: &mut Vec<u8>, value: &T) {
    value
        .consensus_encode(bytes)
        .expect("writing consensus data to a Vec cannot fail");
}

fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    sha256::Hash::hash(bytes).to_byte_array()
}

fn tagged_hash(message: &[u8]) -> [u8; 32] {
    let tag_hash = sha256_bytes(UNIFIED_SIGHASH_TAG);
    let mut tagged_preimage = Vec::with_capacity(64 + message.len());
    tagged_preimage.extend_from_slice(&tag_hash);
    tagged_preimage.extend_from_slice(&tag_hash);
    tagged_preimage.extend_from_slice(message);
    sha256_bytes(&tagged_preimage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use miniscript::bitcoin::{
        absolute, consensus::deserialize, hex::FromHex, transaction::Version, Amount, OutPoint,
        ScriptBuf, Sequence, TxIn, Txid, Witness,
    };
    use serde_json::Value;

    #[test]
    fn matches_all_applicable_upstream_vectors() {
        let rows: Vec<Value> =
            serde_json::from_str(include_str!("../tests/data/unified_sighash.json")).unwrap();
        let mut checked = 0;
        let mut skipped = 0;

        for (fixture_index, row) in rows.into_iter().skip(1).enumerate() {
            let fields = row.as_array().unwrap();
            let script_type = fields[4].as_u64().unwrap() as u8;
            if script_type > SCRIPT_TYPE_WITNESS_V0 {
                skipped += 1;
                continue;
            }

            let script_code =
                ScriptBuf::from_bytes(Vec::from_hex(fields[0].as_str().unwrap()).unwrap());
            let transaction: Transaction =
                deserialize(&Vec::from_hex(fields[1].as_str().unwrap()).unwrap()).unwrap();
            let input_index = fields[2].as_u64().unwrap() as usize;
            let hash_type = fields[3].as_u64().unwrap() as u8;
            let spent_outputs = fields[5]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| {
                    let output = value.as_array().unwrap();
                    TxOut {
                        value: Amount::from_sat(output[0].as_u64().unwrap()),
                        script_pubkey: ScriptBuf::from_bytes(
                            Vec::from_hex(output[1].as_str().unwrap()).unwrap(),
                        ),
                    }
                })
                .collect::<Vec<_>>();
            let expected = <[u8; 32]>::from_hex(fields[6].as_str().unwrap()).unwrap();

            let actual = unified_sighash(
                &transaction,
                input_index,
                hash_type,
                script_type,
                &spent_outputs,
                script_code.as_script(),
            )
            .unwrap_or_else(|error| panic!("upstream vector {}: {error}", fixture_index + 1));
            assert_eq!(actual, expected, "upstream vector {}", fixture_index + 1);
            checked += 1;
        }

        assert_eq!(checked, 142);
        assert_eq!(skipped, 24); // Taproot key-path and tapscript are out of scope.
    }

    #[test]
    fn message_layout_is_byte_exact() {
        let (transaction, spent_outputs, script_code) = simple_transaction();
        let cache = UnifiedSighashCache::new(&transaction, &spent_outputs).unwrap();
        let message =
            unified_sighash_message(&cache, 0, 0x21, SCRIPT_TYPE_BASE, script_code.as_script())
                .unwrap();

        let expected = Vec::from_hex(
            "002102000000110000000071c99cc3bc21757feed5b712744ebb0f770d5c41d99189f9457495747bf11050f76343dc4d5d9507cf73e8036cf96a18a66b4f85ffb3f5a30d27889d3e7246dc877f3713268cdab175893935cd58e2e5c9830c1f4bd1d84995ffc2b7b60b9e03b4248c210a2905b94345e1a8414d0e12efcfb2f4f0f2397159a71283397a0ccd76142dce0ac935dec7d84e883eb1e081eb0148db999ba420bab0527ba8362fd400000000000151",
        )
        .unwrap();
        assert_eq!(message, expected);
        assert_eq!(
            unified_sighash(
                &transaction,
                0,
                0x21,
                SCRIPT_TYPE_BASE,
                &spent_outputs,
                script_code.as_script(),
            )
            .unwrap(),
            <[u8; 32]>::from_hex(
                "2ffea03f809383606b211b594b45afad53a96e034d229e5ed1b52025439e5457"
            )
            .unwrap()
        );
    }

    #[test]
    fn rejects_invalid_context() {
        let (transaction, spent_outputs, script_code) = simple_transaction();

        assert_eq!(
            unified_sighash(
                &transaction,
                0,
                0x01,
                SCRIPT_TYPE_BASE,
                &spent_outputs,
                script_code.as_script(),
            ),
            Err(UnifiedSighashError::MissingUnifiedFlag(0x01))
        );
        assert_eq!(
            unified_sighash(
                &transaction,
                0,
                0x21,
                2,
                &spent_outputs,
                script_code.as_script(),
            ),
            Err(UnifiedSighashError::UnsupportedScriptType(2))
        );
        assert_eq!(
            unified_sighash(
                &transaction,
                0,
                0x21,
                SCRIPT_TYPE_BASE,
                &[],
                script_code.as_script(),
            ),
            Err(UnifiedSighashError::PrevoutsLengthMismatch {
                inputs: 1,
                prevouts: 0,
            })
        );
        assert_eq!(
            unified_sighash(
                &transaction,
                1,
                0x21,
                SCRIPT_TYPE_BASE,
                &spent_outputs,
                script_code.as_script(),
            ),
            Err(UnifiedSighashError::InputIndexOutOfBounds {
                index: 1,
                inputs: 1,
            })
        );

        let mut no_outputs = transaction;
        no_outputs.output.clear();
        assert_eq!(
            unified_sighash(
                &no_outputs,
                0,
                0x23,
                SCRIPT_TYPE_WITNESS_V0,
                &spent_outputs,
                script_code.as_script(),
            ),
            Err(UnifiedSighashError::MissingSingleOutput {
                index: 0,
                outputs: 0,
            })
        );
    }

    fn simple_transaction() -> (Transaction, Vec<TxOut>, ScriptBuf) {
        let script = ScriptBuf::from_bytes(vec![0x51]);
        (
            Transaction {
                version: Version::TWO,
                lock_time: absolute::LockTime::from_consensus(17),
                input: vec![TxIn {
                    previous_output: OutPoint {
                        txid: Txid::all_zeros(),
                        vout: 1,
                    },
                    script_sig: ScriptBuf::new(),
                    sequence: Sequence(0xffff_fffe),
                    witness: Witness::new(),
                }],
                output: vec![TxOut {
                    value: Amount::from_sat(1_000),
                    script_pubkey: script.clone(),
                }],
            },
            vec![TxOut {
                value: Amount::from_sat(5_000),
                script_pubkey: script.clone(),
            }],
            script,
        )
    }
}
